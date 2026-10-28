; Inno Setup script for the Oxide Windows installer.
; Built by .github/workflows/release.yml:  iscc /DAppVersion=x.y.z /DSourceDir=<dir> oxide.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\target\release"
#endif

[Setup]
AppId={{6C0F5E2B-6E7A-4D5C-9C0B-5B0E7E9A0D11}
AppName=Oxide
AppVersion={#AppVersion}
AppPublisher=Oxide contributors
DefaultDirName={autopf}\Oxide
DefaultGroupName=Oxide
UninstallDisplayIcon={app}\oxide-gui.exe
OutputBaseFilename=Oxide-{#AppVersion}-windows-x64-setup
SetupIconFile=..\..\assets\oxide.ico
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesAssociations=yes
ChangesEnvironment=yes
PrivilegesRequiredOverridesAllowed=dialog
LicenseFile=..\..\LICENSE-MIT

[Tasks]
Name: "associate"; Description: "Open .zip, .7z, .tar, .gz, .xz, .zst and .rar files with Oxide"; Flags: unchecked
Name: "addtopath"; Description: "Add the oxide command-line tool to PATH"

[Files]
Source: "{#SourceDir}\oxide-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\oxide.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: isreadme
Source: "..\..\LICENSE-MIT"; DestDir: "{app}"
Source: "..\..\LICENSE-APACHE"; DestDir: "{app}"

[Icons]
Name: "{group}\Oxide"; Filename: "{app}\oxide-gui.exe"
Name: "{group}\Uninstall Oxide"; Filename: "{uninstallexe}"

[Registry]
Root: HKA; Subkey: "Software\Classes\Oxide.Archive"; ValueType: string; ValueData: "Archive"; Flags: uninsdeletekey; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Oxide.Archive\DefaultIcon"; ValueType: string; ValueData: "{app}\oxide-gui.exe,0"; Tasks: associate
Root: HKA; Subkey: "Software\Classes\Oxide.Archive\shell\open\command"; ValueType: string; ValueData: """{app}\oxide-gui.exe"" ""%1"""; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.zip\OpenWithProgids"; ValueType: string; ValueName: "Oxide.Archive"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.7z\OpenWithProgids"; ValueType: string; ValueName: "Oxide.Archive"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.tar\OpenWithProgids"; ValueType: string; ValueName: "Oxide.Archive"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.gz\OpenWithProgids"; ValueType: string; ValueName: "Oxide.Archive"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.tgz\OpenWithProgids"; ValueType: string; ValueName: "Oxide.Archive"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.xz\OpenWithProgids"; ValueType: string; ValueName: "Oxide.Archive"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.bz2\OpenWithProgids"; ValueType: string; ValueName: "Oxide.Archive"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.zst\OpenWithProgids"; ValueType: string; ValueName: "Oxide.Archive"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Software\Classes\.rar\OpenWithProgids"; ValueType: string; ValueName: "Oxide.Archive"; ValueData: ""; Flags: uninsdeletevalue; Tasks: associate
Root: HKA; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

[Code]
function NeedsAddPath(Dir: string): Boolean;
var
  Paths: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') = 0;
end;
