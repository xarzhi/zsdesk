; ZSDesk 安装包脚本（Inno Setup 6）。
; 由仓库根目录的 package.ps1 调用，路径与版本号通过 /D 传入：
;   ISCC /DSourceDir=<payload> /DOutputDir=<releases\版本> /DMyAppVersion=<版本> setup.iss
; 本文件保持纯 ASCII，中文界面由编译器自带的 ChineseSimplified.isl 提供，
; 避免 ISCC 按 ANSI 代码页读取源文件造成乱码。

#ifndef SourceDir
  #error SourceDir must point at the built payload
#endif
#ifndef OutputDir
  #error OutputDir must point at the release folder
#endif
#ifndef MyAppVersion
  #error MyAppVersion is required
#endif

#define MyAppName "ZSDesk"
#define MyAppExe "ZSDesk.exe"
#define MyAppPublisher "ZSDesk contributors"
#define MyAppUrl "https://github.com/qiu7824/zsdesk"

[Setup]
; 与 scripts/package.ps1 的全量版共用 AppId，升级时会被识别为同一个程序。
AppId={{470DF6E5-CCB7-4A3F-9ED0-F68009D5A91A}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppUrl}
AppSupportURL={#MyAppUrl}/issues
AppUpdatesURL={#MyAppUrl}/releases
DefaultDirName={autopf}\ZSDesk
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
UsePreviousAppDir=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir={#OutputDir}
OutputBaseFilename=ZSDesk-{#MyAppVersion}-x64-setup
SetupIconFile=..\..\crates\zsdesk-app\resources\zsdesk.ico
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
UninstallDisplayIcon={app}\{#MyAppExe}
UninstallDisplayName={#MyAppName} {#MyAppVersion}
AppMutex=Local\ZSDesk.Native.Application
CloseApplications=yes
RestartApplications=no
ShowLanguageDialog=auto
LanguageDetectionMethod=uilanguage
VersionInfoDescription={#MyAppName} {#MyAppVersion} Setup
VersionInfoProductName={#MyAppName}
VersionInfoCompany={#MyAppPublisher}

[Languages]
Name: "chinesesimp"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
; A desktop shortcut is what most people look for after installing, so the box
; starts ticked; it stays optional because the start-menu entry is always made.
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"; AppUserModelID: "ZSDesk.Native"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"; AppUserModelID: "ZSDesk.Native"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExe}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent
