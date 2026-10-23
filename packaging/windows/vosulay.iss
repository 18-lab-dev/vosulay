; Compile with ISCC /DAppVersion=<version> /DSourceDir=<package> /DOutputDir=<dist> vosulay.iss.
; The staged ZIP layout has no portable marker; settings remain in the user config directory.
#ifndef AppVersion
  #error AppVersion must be supplied with /DAppVersion=.
#endif
#ifndef SourceDir
  #error SourceDir must be supplied with /DSourceDir=.
#endif
#ifndef OutputDir
  #error OutputDir must be supplied with /DOutputDir=.
#endif

[Setup]
AppId={{D1A97E8C-F65B-44D8-8E1B-25BC7A83E294}
AppName=Vosulay
AppPublisher=18-LAB
AppVersion={#AppVersion}
; Code signing requires the product name and version on every signed file,
; matching vosulay.exe and vosulay.dll.
VersionInfoVersion={#AppVersion}
VersionInfoProductName=Vosulay
VersionInfoProductVersion={#AppVersion}
VersionInfoProductTextVersion={#AppVersion}
VersionInfoCompany=18-LAB
VersionInfoDescription=Vosulay Setup
DefaultDirName={localappdata}\Programs\Vosulay
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
LicenseFile=..\..\LICENSE
SetupIconFile=..\..\assets\branding\vosulay.ico
OutputDir={#OutputDir}
OutputBaseFilename=vosulay-{#AppVersion}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
UninstallDisplayIcon={app}\vosulay.exe
; Matches the application's session-local single-instance mutex. Exit via the tray before upgrading.
AppMutex=Local\vosulay-instance
DisableProgramGroupPage=yes

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; Flags: unchecked
Name: "startup"; Description: "Start Vosulay when &Windows starts"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
; The application self-registers this exact per-user path and target.
Name: "{userprograms}\Vosulay"; Filename: "{app}\vosulay.exe"; WorkingDir: "{app}"
Name: "{userdesktop}\Vosulay"; Filename: "{app}\vosulay.exe"; WorkingDir: "{app}"; Tasks: desktopicon
Name: "{userstartup}\Vosulay"; Filename: "{app}\vosulay.exe"; WorkingDir: "{app}"; Tasks: startup

[Run]
Filename: "{app}\vosulay.exe"; Description: "Launch &Vosulay"; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent

; No [UninstallDelete]: user settings and profiles under %APPDATA%\vosulay are retained.
