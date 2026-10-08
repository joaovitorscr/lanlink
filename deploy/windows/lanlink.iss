; Inno Setup script for the lanlink Windows installer. Built by .github/workflows/release.yml:
;   ISCC.exe /DAppVersion=<version> /DSourceDir=<dir with lanlink.exe, lanlink-cli.exe> \
;            /DOutputDir=<dir> /DOutputBase=<file name without .exe> deploy\windows\lanlink.iss
; Installs per-user by default (no admin prompt), into %LOCALAPPDATA%\Programs\lanlink.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\dist"
#endif
#ifndef OutputDir
  #define OutputDir "..\.."
#endif
#ifndef OutputBase
  #define OutputBase "lanlink-setup"
#endif

[Setup]
AppId={{7E2C0E6A-3B5D-4F1B-9C0C-6D2A1F4E8B10}
AppName=lanlink
AppVersion={#AppVersion}
AppPublisher=lanlink
AppPublisherURL=https://github.com/joaovitorscr/lanlink
AppSupportURL=https://github.com/joaovitorscr/lanlink/issues
DefaultDirName={autopf}\lanlink
DefaultGroupName=lanlink
UninstallDisplayIcon={app}\lanlink.exe
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBase}
SetupIconFile=..\..\assets\icon.ico
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; lanlink keeps running in the notification area; ask to close it before replacing the exe.
CloseApplications=yes
RestartApplications=no

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\lanlink.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\lanlink-cli.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\lanlink"; Filename: "{app}\lanlink.exe"
Name: "{group}\Uninstall lanlink"; Filename: "{uninstallexe}"
Name: "{autodesktop}\lanlink"; Filename: "{app}\lanlink.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\lanlink.exe"; Description: "{cm:LaunchProgram,lanlink}"; Flags: nowait postinstall skipifsilent
