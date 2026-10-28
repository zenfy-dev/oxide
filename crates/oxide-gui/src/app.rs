//! Application state, background jobs, and the actions behind every button.
//!
//! Long operations run on a worker thread that calls straight into
//! `oxide-core`; the UI thread only reads progress snapshots.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use eframe::egui;
use oxide_core::{
    ArchiveFormat, ArchiveInfo, CancelToken, CreateOptions, ExtractOptions, OperationSummary, OxideError, Password,
    Progress, ProgressUpdate, ReadOptions,
};

use crate::components::create_dialog::CreateDialog;
use crate::components::extract_dialog::ExtractDialog;
use crate::components::file_table::{Row, SortKey, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Open,
    Create,
    Extract,
    Test,
    Add,
}

pub enum JobOutput {
    Listing(ArchiveInfo),
    Summary(OperationSummary, PathBuf),
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
}

pub struct OpenArchive {
    pub info: ArchiveInfo,
    pub tree: Tree,
    pub cwd: String,
    pub back: Vec<String>,
    pub selection: HashSet<String>,
    pub anchor: Option<usize>,
    pub sort: SortKey,
    pub ascending: bool,
    pub rows: Vec<Row>,
    pub rows_key: Option<(String, String, SortKey, bool)>,
}

impl OpenArchive {
    fn new(info: ArchiveInfo) -> Self {
        let tree = Tree::build(&info.entries);
        // Skip a lone top-level folder, as most archives wrap their contents in one.
        let cwd = tree.single_root_dir().unwrap_or_default();
        OpenArchive {
            info,
            tree,
            cwd,
            back: Vec::new(),
            selection: HashSet::new(),
            anchor: None,
            sort: SortKey::Name,
            ascending: true,
            rows: Vec::new(),
            rows_key: None,
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
}

pub struct Toast {
    pub text: String,
    pub until: Instant,
}

pub struct PasswordPrompt {
    pub text: String,
    pub wrong: bool,
}

pub struct OxideApp {
    pub archive: Option<OpenArchive>,
    pub job: Option<Job>,
    pub create: Option<CreateDialog>,
    pub extract: Option<ExtractDialog>,
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
    #[cfg(debug_assertions)]
    capture: Option<crate::devtools::Capture>,
}

impl OxideApp {
    pub fn new(ctx: &egui::Context, initial: Option<PathBuf>) -> Self {
        let mut app = OxideApp {
            archive: None,
            job: None,
            create: None,
            extract: None,
            password_prompt: None,
            drop_choice: None,
            error: None,
            warnings: Vec::new(),
            toast: None,
            password: None,
            last_action: None,
            search: String::new(),
            focus_search: false,
            theme: egui::ThemePreference::System,
            #[cfg(debug_assertions)]
            capture: crate::devtools::from_env(ctx),
        };
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
            Action::Extract { archive, dest, mut options } => {
                if options.password.is_none() {
                    options.password = password;
                }
                let title = format!("Extracting {}", file_name(&archive));
                self.spawn(ctx, JobKind::Extract, title, move |progress| {
                    oxide_core::extract(&archive, &dest, &options, progress).map(|s| JobOutput::Summary(s, dest))
                });
            }
        }
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
                self.password_prompt =
                    Some(PasswordPrompt { text: String::new(), wrong: matches!(e, OxideError::WrongPassword) });
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn job_succeeded(&mut self, ctx: &egui::Context, kind: JobKind, output: JobOutput) {
        match output {
            JobOutput::Listing(info) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{} — Oxide", file_name(&info.path))));
                self.search.clear();
                self.archive = Some(OpenArchive::new(info));
            }
            JobOutput::Summary(summary, target) => {
                self.warnings = summary.warnings.clone();
                match kind {
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
                    JobKind::Open => {}
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
        if paths.is_empty() || self.busy() {
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_job(&ctx);
        #[cfg(debug_assertions)]
        crate::devtools::apply_demo(self, &ctx);
        crate::ui::draw(self, ui);
        #[cfg(debug_assertions)]
        if let Some(capture) = &mut self.capture {
            capture.tick(&ctx);
        }
    }
}
