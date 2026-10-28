//! Application state, background jobs, and the actions behind every button.
//!
//! Long operations run on a worker thread that calls straight into
//! `oxide-core`; the UI thread only reads progress snapshots.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use eframe::egui;
use oxide_core::{
    ArchiveFormat, ArchiveInfo, CancelToken, CreateOptions, Entry, EntryKind, ExtractOptions, OperationSummary,
    OxideError, Password, Progress, ProgressUpdate, ReadOptions,
};

use crate::components::create_dialog::CreateDialog;
use crate::components::extract_dialog::ExtractDialog;
use crate::components::file_table::{Row, SortKey, Tree};
use crate::components::preview::Preview;
use crate::file_access::{self, FileIntent, PreparedFile, StagedFile};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Open,
    Create,
    Extract,
    Test,
    Add,
    /// Extract beside the archive (files opened from Finder).
    QuickExtract,
    File,
}

/// Whether the window is shown, decided at launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Window hidden until we know whether Finder handed us files.
    Deciding(Instant),
    /// Normal app with a window.
    Window,
    /// Launched by opening archives from Finder: extract them, then quit.
    /// The window only appears if a password or an error needs the user.
    Background,
}

/// How long to wait at launch for Finder's "open" message before showing the window.
const LAUNCH_GRACE: Duration = Duration::from_millis(150);

pub enum JobOutput {
    Listing(ArchiveInfo),
    Summary(OperationSummary, PathBuf),
    PreviewFile(Box<PreparedFile>),
    OpenedFile(StagedFile),
}

#[derive(Default)]
pub struct JobShared {
    pub update: Option<ProgressUpdate>,
    pub outcome: Option<Result<JobOutput, OxideError>>,
}

/// A running background operation.
pub struct Job {
    pub kind: JobKind,
    pub title: String,
    pub cancel: CancelToken,
    pub shared: Arc<Mutex<JobShared>>,
    pub started: Instant,
}

impl Job {
    pub fn shared(&self) -> MutexGuard<'_, JobShared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// An operation that can be re-run after the user supplies a password.
#[derive(Clone)]
pub enum Action {
    Open(PathBuf),
    Extract { archive: PathBuf, dest: PathBuf, options: ExtractOptions },
    Test(PathBuf),
    QuickExtract(PathBuf),
    File { archive: PathBuf, entry: Entry, intent: FileIntent },
}

pub struct OpenArchive {
    pub info: ArchiveInfo,
    pub stats: ArchiveStats,
    pub tree: Tree,
    pub cwd: String,
    pub back: Vec<String>,
    pub selection: HashSet<String>,
    pub anchor: Option<usize>,
    pub sort: SortKey,
    pub ascending: bool,
    pub rows: Vec<Row>,
    pub rows_key: Option<(String, String, SortKey, bool)>,
    /// Row to scroll into view on the next frame (keyboard navigation).
    pub scroll_to: Option<usize>,
}

/// Whole-archive totals are immutable while browsing; compute them once.
pub struct ArchiveStats {
    pub files: usize,
    pub total_size: u64,
    pub encrypted: bool,
    pub compression_ratio: Option<f64>,
}

impl OpenArchive {
    fn new(info: ArchiveInfo) -> Self {
        let tree = Tree::build(&info.entries);
        // Skip a lone top-level folder, as most archives wrap their contents in one.
        let cwd = tree.single_root_dir().unwrap_or_default();
        let stats = ArchiveStats {
            files: info.file_count(),
            total_size: info.total_size(),
            encrypted: info.is_encrypted(),
            compression_ratio: info.compression_ratio(),
        };
        OpenArchive {
            info,
            stats,
            tree,
            cwd,
            back: Vec::new(),
            selection: HashSet::new(),
            anchor: None,
            sort: SortKey::Name,
            ascending: true,
            rows: Vec::new(),
            rows_key: None,
            scroll_to: None,
        }
    }

    pub fn navigate(&mut self, dir: String) {
        if dir != self.cwd {
            self.back.push(std::mem::replace(&mut self.cwd, dir));
            self.selection.clear();
            self.anchor = None;
        }
    }

    pub fn go_back(&mut self) {
        if let Some(prev) = self.back.pop() {
            self.cwd = prev;
            self.selection.clear();
        }
    }

    pub fn go_up(&mut self) {
        if !self.cwd.is_empty() {
            let parent = self.cwd.rsplit_once('/').map(|(p, _)| p.to_owned()).unwrap_or_default();
            self.navigate(parent);
        }
    }

    /// Recomputes the visible rows when the folder, search or sort changes.
    pub fn refresh_rows(&mut self, search: &str) {
        let key = (self.cwd.clone(), search.to_owned(), self.sort, self.ascending);
        if self.rows_key.as_ref() == Some(&key) {
            return;
        }
        self.rows = if search.trim().is_empty() {
            self.tree.children(&self.cwd).to_vec()
        } else {
            self.tree.search(search.trim())
        };
        crate::components::file_table::sort_rows(&mut self.rows, self.sort, self.ascending);
        self.rows_key = Some(key);
    }

    pub fn supports_add(&self) -> bool {
        self.info.format == ArchiveFormat::Zip || self.info.format.is_tar()
    }

    pub fn selected_file(&self) -> Option<&str> {
        if self.selection.len() != 1 {
            return None;
        }
        self.rows.iter().find(|row| self.selection.contains(&row.path))?.file_path.as_deref()
    }
}

pub struct Toast {
    pub text: String,
    pub until: Instant,
}

pub struct PasswordPrompt {
    pub text: String,
    pub wrong: bool,
}

/// Remembered between launches.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Settings {
    theme: String,
    recent: Vec<PathBuf>,
}

const SETTINGS_KEY: &str = "oxide.settings";
const MAX_RECENT: usize = 8;

pub struct OxideApp {
    pub archive: Option<OpenArchive>,
    pub job: Option<Job>,
    pub create: Option<CreateDialog>,
    pub extract: Option<ExtractDialog>,
    pub preview: Option<Preview>,
    /// External applications may read lazily; retain these copies until exit.
    opened_files: Vec<StagedFile>,
    pub password_prompt: Option<PasswordPrompt>,
    /// Files dropped onto an open archive, awaiting "add or create" choice.
    pub drop_choice: Option<Vec<PathBuf>>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub toast: Option<Toast>,
    pub password: Option<Password>,
    pub last_action: Option<Action>,
    pub search: String,
    pub focus_search: bool,
    pub theme: egui::ThemePreference,
    /// Recently opened archives, newest first.
    pub recent: Vec<PathBuf>,
    /// The app icon as a texture, for the empty state.
    pub icon: Option<egui::TextureHandle>,
    pub mode: LaunchMode,
    /// Archives opened from Finder, waiting to be extracted.
    pub quick_queue: VecDeque<PathBuf>,
    #[cfg(debug_assertions)]
    capture: Option<crate::devtools::Capture>,
}

impl OxideApp {
    pub fn new(ctx: &egui::Context, storage: Option<&dyn eframe::Storage>, initial: Option<PathBuf>) -> Self {
        let settings: Settings = storage.and_then(|s| eframe::get_value(s, SETTINGS_KEY)).unwrap_or_default();
        let theme = match settings.theme.as_str() {
            "light" => egui::ThemePreference::Light,
            "dark" => egui::ThemePreference::Dark,
            _ => egui::ThemePreference::System,
        };
        ctx.set_theme(theme);
        let mut app = OxideApp {
            archive: None,
            job: None,
            create: None,
            extract: None,
            preview: None,
            opened_files: Vec::new(),
            password_prompt: None,
            drop_choice: None,
            error: None,
            warnings: Vec::new(),
            toast: None,
            password: None,
            last_action: None,
            search: String::new(),
            focus_search: false,
            theme,
            recent: settings.recent,
            icon: None,
            // Only macOS delivers opened files after launch; elsewhere the window shows at once.
            mode: if cfg!(target_os = "macos") && initial.is_none() {
                LaunchMode::Deciding(Instant::now())
            } else {
                LaunchMode::Window
            },
            quick_queue: VecDeque::new(),
            #[cfg(debug_assertions)]
            capture: crate::devtools::from_env(ctx),
        };
        ctx.request_repaint();
        if let Some(path) = initial {
            if path.is_file() {
                app.open_path(ctx, path);
            } else {
                app.create = Some(CreateDialog::new(vec![path]));
            }
        }
        app
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn notify(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast { text: text.into(), until: Instant::now() + Duration::from_secs(5) });
    }

    /// Starts `work` on a worker thread with progress and cancellation wired up.
    fn spawn(
        &mut self,
        ctx: &egui::Context,
        kind: JobKind,
        title: String,
        work: impl FnOnce(&Progress) -> Result<JobOutput, OxideError> + Send + 'static,
    ) {
        let cancel = CancelToken::new();
        let shared = Arc::new(Mutex::new(JobShared::default()));
        let (sink, repaint) = (shared.clone(), ctx.clone());
        let progress = Progress::with_callback(move |u| {
            sink.lock().unwrap_or_else(|e| e.into_inner()).update = Some(u.clone());
            repaint.request_repaint();
        })
        .cancel_with(cancel.clone())
        .throttle(Duration::from_millis(50));

        let (done, repaint) = (shared.clone(), ctx.clone());
        let spawned = std::thread::Builder::new().name("oxide-job".into()).spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&progress)))
                .unwrap_or_else(|_| Err(std::io::Error::other("the operation stopped unexpectedly").into()));
            done.lock().unwrap_or_else(|e| e.into_inner()).outcome = Some(outcome);
            repaint.request_repaint();
        });
        if let Err(e) = spawned {
            self.error = Some(format!("Could not start a worker thread: {e}"));
            return;
        }
        self.job = Some(Job { kind, title, cancel, shared, started: Instant::now() });
    }

    /// Opens an archive chosen by the user (forgets any previous password).
    pub fn open_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        tracing::info!(path = %path.display(), "opening archive");
        self.password = None;
        self.run(ctx, Action::Open(path));
    }

    pub fn run(&mut self, ctx: &egui::Context, action: Action) {
        if self.busy() {
            return;
        }
        self.last_action = Some(action.clone());
        let password = self.password.clone();
        match action {
            Action::Open(path) => {
                let title = format!("Opening {}", file_name(&path));
                self.spawn(ctx, JobKind::Open, title, move |progress| {
                    oxide_core::list_with_progress(&path, &ReadOptions { password, format: None }, progress)
                        .map(JobOutput::Listing)
                });
            }
            Action::Test(path) => {
                let title = format!("Testing {}", file_name(&path));
                self.spawn(ctx, JobKind::Test, title, move |progress| {
                    oxide_core::test(&path, &ReadOptions { password, format: None }, progress)
                        .map(|s| JobOutput::Summary(s, path))
                });
            }
            Action::QuickExtract(archive) => {
                let title = format!("Extracting {}", file_name(&archive));
                self.spawn(ctx, JobKind::QuickExtract, title, move |progress| {
                    crate::quick_extract::extract_beside(&archive, password, progress)
                        .map(|(s, target)| JobOutput::Summary(s, target))
                });
            }
            Action::Extract { archive, dest, mut options } => {
                if options.password.is_none() {
                    options.password = password;
                }
                let title = format!("Extracting {}", file_name(&archive));
                self.spawn(ctx, JobKind::Extract, title, move |progress| {
                    oxide_core::extract(&archive, &dest, &options, progress).map(|s| JobOutput::Summary(s, dest))
                });
            }
            Action::File { archive, entry, intent } => {
                let verb = if intent == FileIntent::Open { "Opening" } else { "Previewing" };
                let title = format!("{verb} {}", entry.name());
                self.spawn(ctx, JobKind::File, title, move |progress| {
                    let prepared = file_access::prepare(&archive, &entry, password, intent, progress)?;
                    match intent {
                        FileIntent::Open => {
                            progress.cancel_token().check()?;
                            file_access::open_default(&prepared.file)?;
                            Ok(JobOutput::OpenedFile(prepared.file))
                        }
                        FileIntent::Preview => Ok(JobOutput::PreviewFile(Box::new(prepared))),
                    }
                });
            }
        }
    }

    pub fn open_entry(&mut self, ctx: &egui::Context, path: &str, intent: FileIntent) {
        let Some(archive) = &self.archive else { return };
        let Some(entry) = archive.info.entries.iter().find(|entry| entry.path == path && entry.kind == EntryKind::File)
        else {
            self.error = Some("Only regular files can be opened or previewed.".into());
            return;
        };
        self.run(ctx, Action::File { archive: archive.info.path.clone(), entry: entry.clone(), intent });
    }

    pub fn preview_selected(&mut self, ctx: &egui::Context) {
        if let Some(path) = self.archive.as_ref().and_then(OpenArchive::selected_file).map(str::to_owned) {
            self.open_entry(ctx, &path, FileIntent::Preview);
        }
    }

    pub fn open_staged(&mut self, ctx: &egui::Context, file: StagedFile) {
        if self.busy() {
            return;
        }
        let title = format!("Opening {}", file.name);
        self.spawn(ctx, JobKind::File, title, move |progress| {
            progress.cancel_token().check()?;
            file_access::open_default(&file)?;
            Ok(JobOutput::OpenedFile(file))
        });
    }

    pub fn start_create(&mut self, ctx: &egui::Context, inputs: Vec<PathBuf>, output: PathBuf, options: CreateOptions) {
        let title = format!("Creating {}", file_name(&output));
        self.spawn(ctx, JobKind::Create, title, move |progress| {
            oxide_core::create(&inputs, &output, &options, progress).map(|s| JobOutput::Summary(s, output))
        });
    }

    pub fn start_add(&mut self, ctx: &egui::Context, inputs: Vec<PathBuf>) {
        let Some(archive) = self.archive.as_ref().map(|a| a.info.path.clone()) else {
            return;
        };
        let options = CreateOptions { password: self.password.clone(), ..CreateOptions::default() };
        let title = format!("Adding to {}", file_name(&archive));
        self.spawn(ctx, JobKind::Add, title, move |progress| {
            oxide_core::add(&archive, &inputs, &options, progress).map(|s| JobOutput::Summary(s, archive))
        });
    }

    /// Collects a finished job's result, if any.
    pub fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else {
            return;
        };
        let Some(outcome) = job.shared().outcome.take() else {
            return;
        };
        let kind = job.kind;
        self.job = None;
        match outcome {
            Ok(output) => self.job_succeeded(ctx, kind, output),
            Err(OxideError::Cancelled) => self.notify("Cancelled — nothing was left half-written"),
            Err(e @ (OxideError::PasswordRequired | OxideError::WrongPassword)) if self.last_action.is_some() => {
                self.show_window(ctx);
                self.password_prompt =
                    Some(PasswordPrompt { text: String::new(), wrong: matches!(e, OxideError::WrongPassword) });
            }
            Err(e) => {
                self.show_window(ctx);
                let what = match (&self.last_action, kind) {
                    (Some(Action::QuickExtract(path)), JobKind::QuickExtract) => {
                        format!("Couldn't extract {}: ", file_name(path))
                    }
                    _ => String::new(),
                };
                self.error = Some(format!("{what}{e}"));
            }
        }
    }

    /// Makes the window visible (it starts hidden on macOS).
    pub fn show_window(&mut self, ctx: &egui::Context) {
        tracing::info!("showing window");
        if matches!(self.mode, LaunchMode::Deciding(_)) {
            self.mode = LaunchMode::Window;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Launch decision, Finder opens, and quitting after background extraction.
    fn drive_launch(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "macos")]
        self.quick_queue.extend(crate::macos_open::take_opened());

        if let LaunchMode::Deciding(started) = self.mode {
            if !self.quick_queue.is_empty() {
                self.mode = LaunchMode::Background;
            } else if started.elapsed() >= LAUNCH_GRACE {
                self.show_window(ctx);
            } else {
                ctx.request_repaint_after(Duration::from_millis(25));
            }
        }
        if matches!(self.mode, LaunchMode::Deciding(_)) {
            return;
        }
        let waiting_on_user = self.password_prompt.is_some() || self.error.is_some();
        if !self.busy()
            && !waiting_on_user
            && let Some(next) = self.quick_queue.pop_front()
        {
            self.password = None;
            self.run(ctx, Action::QuickExtract(next));
        }
        if self.mode == LaunchMode::Background && !self.busy() && !waiting_on_user && self.quick_queue.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn job_succeeded(&mut self, ctx: &egui::Context, kind: JobKind, output: JobOutput) {
        match output {
            JobOutput::Listing(info) => {
                tracing::info!(path = %info.path.display(), entries = info.entries.len(), "archive opened");
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{} — Oxide", file_name(&info.path))));
                self.search.clear();
                self.preview = None;
                self.remember(info.path.clone());
                self.archive = Some(OpenArchive::new(info));
            }
            JobOutput::Summary(summary, target) => {
                self.warnings = summary.warnings.clone();
                match kind {
                    JobKind::QuickExtract => {
                        tracing::info!(target = %target.display(), files = summary.files, "extracted beside archive");
                        // In the background there is no window to show warnings in.
                        if self.mode == LaunchMode::Background {
                            self.warnings.clear();
                        } else {
                            self.notify(format!("Extracted to {}", file_name(&target)));
                        }
                    }
                    JobKind::Extract => self.notify(format!(
                        "Extracted {} files ({}) to {}",
                        summary.files,
                        crate::components::human_bytes(summary.bytes),
                        target.display()
                    )),
                    JobKind::Test => self.notify(format!(
                        "No errors found · {} files ({}) verified",
                        summary.files,
                        crate::components::human_bytes(summary.bytes)
                    )),
                    JobKind::Create => {
                        self.notify(format!("Created {}", file_name(&target)));
                        self.open_path(ctx, target);
                    }
                    JobKind::Add => {
                        self.notify(format!("Added {} items", summary.files + summary.directories));
                        self.run(ctx, Action::Open(target));
                    }
                    JobKind::Open | JobKind::File => {}
                }
            }
            JobOutput::PreviewFile(prepared) => self.preview = Some(Preview::new(*prepared)),
            JobOutput::OpenedFile(file) => {
                self.notify(format!("Opened {} · edits won’t update the archive", file.name));
                if !self.opened_files.iter().any(|opened| opened.path == file.path) {
                    self.opened_files.push(file);
                }
            }
        }
    }

    /// Re-runs the operation that needed a password.
    pub fn retry_with_password(&mut self, ctx: &egui::Context, password: Password) {
        self.password = Some(password.clone());
        if let Some(mut action) = self.last_action.take() {
            if let Action::Extract { options, .. } = &mut action {
                options.password = Some(password);
            }
            self.run(ctx, action);
        }
    }

    fn remember(&mut self, path: PathBuf) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(MAX_RECENT);
    }

    pub fn set_theme(&mut self, ctx: &egui::Context, theme: egui::ThemePreference) {
        self.theme = theme;
        ctx.set_theme(theme);
    }

    pub fn pick_and_open(&mut self, ctx: &egui::Context) {
        let exts: Vec<&str> = ArchiveFormat::ALL.iter().flat_map(|f| f.extensions().iter().copied()).collect();
        let picked = rfd::FileDialog::new()
            .set_title("Open archive")
            .add_filter("Archives", &exts)
            .add_filter("All files", &["*"])
            .pick_file();
        if let Some(path) = picked {
            self.open_path(ctx, path);
        }
    }

    pub fn open_create_dialog(&mut self, inputs: Vec<PathBuf>) {
        self.create = Some(CreateDialog::new(inputs));
    }

    pub fn open_extract_dialog(&mut self, only_selected: bool) {
        if let Some(a) = &self.archive {
            let selection: Vec<String> = a.selection.iter().cloned().collect();
            self.extract = Some(ExtractDialog::new(&a.info, selection, only_selected, self.password.as_ref()));
        }
    }

    pub fn test_archive(&mut self, ctx: &egui::Context) {
        if let Some(path) = self.archive.as_ref().map(|a| a.info.path.clone()) {
            self.run(ctx, Action::Test(path));
        }
    }

    pub fn pick_and_add(&mut self, ctx: &egui::Context) {
        if let Some(files) = rfd::FileDialog::new().set_title("Add files to archive").pick_files() {
            self.start_add(ctx, files);
        }
    }

    /// Dropped files: archives open; anything else starts a new archive (or
    /// offers to add to the open one).
    pub fn handle_drop(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        if self.busy() {
            self.notify("Wait for the current operation to finish, then drop again");
            return;
        }
        if paths.len() == 1 && paths[0].is_file() && oxide_core::detect_format(&paths[0]).is_ok() {
            let path = paths.into_iter().next().unwrap_or_default();
            self.open_path(ctx, path);
        } else if self.archive.as_ref().is_some_and(OpenArchive::supports_add) {
            self.drop_choice = Some(paths);
        } else {
            self.open_create_dialog(paths);
        }
    }
}

pub fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

impl eframe::App for OxideApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        #[cfg(debug_assertions)]
        if self.capture.is_some() {
            return;
        }
        let theme = match self.theme {
            egui::ThemePreference::Light => "light",
            egui::ThemePreference::Dark => "dark",
            egui::ThemePreference::System => "system",
        };
        eframe::set_value(storage, SETTINGS_KEY, &Settings { theme: theme.into(), recent: self.recent.clone() });
    }

    /// Runs every frame, and also while the window is hidden.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_job(ctx);
        self.drive_launch(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        #[cfg(debug_assertions)]
        crate::devtools::apply_demo(self, &ui.ctx().clone());
        crate::ui::draw(self, ui);
        #[cfg(debug_assertions)]
        if let Some(capture) = &mut self.capture {
            let waiting_for_preview = std::env::var("OXIDE_DEMO").as_deref() == Ok("preview")
                && self.preview.is_none()
                && self.error.is_none();
            if waiting_for_preview {
                ui.ctx().request_repaint_after(Duration::from_millis(25));
            } else {
                capture.tick(ui.ctx());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(root: &Path, encrypted: bool) -> ArchiveInfo {
        let input = root.join("notes.txt");
        std::fs::write(&input, "archive preview ✓").unwrap();
        let archive = root.join("notes.zip");
        let options = CreateOptions { password: encrypted.then(|| "right".into()), ..Default::default() };
        oxide_core::create(&[input], &archive, &options, &Progress::none()).unwrap();
        oxide_core::list(&archive, &ReadOptions::default()).unwrap()
    }

    fn wait_job(app: &mut OxideApp, ctx: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.busy() {
            assert!(Instant::now() < deadline, "worker did not finish");
            app.poll_job(ctx);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn preview_password_prompt_retries_the_selected_file_and_closing_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let info = archive(tmp.path(), true);
        let ctx = egui::Context::default();
        let mut app = OxideApp::new(&ctx, None, None);
        app.archive = Some(OpenArchive::new(info));
        app.open_entry(&ctx, "notes.txt", FileIntent::Preview);
        wait_job(&mut app, &ctx);
        assert!(app.password_prompt.as_ref().is_some_and(|p| !p.wrong));
        app.password_prompt = None;
        app.retry_with_password(&ctx, "wrong".into());
        wait_job(&mut app, &ctx);
        assert!(app.password_prompt.as_ref().is_some_and(|p| p.wrong));
        app.password_prompt = None;
        app.retry_with_password(&ctx, "right".into());
        wait_job(&mut app, &ctx);
        assert!(app.error.is_none(), "{:?}", app.error);
        let path = app.preview.as_ref().unwrap().file.path.clone();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "archive preview ✓");
        app.preview = None;
        assert!(!path.exists());
    }

    #[test]
    fn opened_copies_survive_archive_changes_and_are_removed_when_the_app_drops() {
        let tmp = tempfile::tempdir().unwrap();
        let info = archive(tmp.path(), false);
        let prepared =
            file_access::prepare(&info.path, &info.entries[0], None, FileIntent::Open, &Progress::none()).unwrap();
        let path = prepared.file.path.clone();
        let ctx = egui::Context::default();
        let mut app = OxideApp::new(&ctx, None, None);
        app.archive = Some(OpenArchive::new(info));
        app.job_succeeded(&ctx, JobKind::File, JobOutput::OpenedFile(prepared.file));
        app.archive = None;
        assert!(path.is_file());
        drop(app);
        assert!(!path.exists());
    }
}
