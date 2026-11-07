; Inno Setup script for Pressa. Build with installer\windows\build.ps1 (or ISCC.exe pressa.iss
; after `cargo build --release`). Output: target\installer\Pressa-Setup-<version>.exe

#define Root AddBackslash(SourcePath) + "..\.."
#define Bin Root + "\target\release"
#ifndef AppVersion
  #define AppVersion GetVersionNumbersString(Bin + "\Pressa.exe")
#endif

[Setup]
AppId={{5B8E2C4A-7F31-4D6B-9A0E-3C2F8D1B6E57}
AppName=Pressa
AppVersion={#AppVersion}
AppVerName=Pressa {#AppVersion}
AppPublisher=Pressa
DefaultDirName={autopf}\Pressa
DefaultGroupName=Pressa
DisableProgramGroupPage=yes
; Per-user install into %LocalAppData%\Programs: no admin prompt.
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#Root}\target\installer
OutputBaseFilename=Pressa-Setup-{#AppVersion}
SetupIconFile={#Root}\assets\pressa.ico
UninstallDisplayIcon={app}\Pressa.exe
UninstallDisplayName=Pressa
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
ChangesAssociations=yes
CloseApplications=yes

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"

[CustomMessages]
en.OpenWith=Open with Pressa
ru.OpenWith=Открыть в Pressa
en.Integration=Windows integration:
ru.Integration=Интеграция с Windows:
en.ContextMenu=Add "Open with Pressa" to the right-click menu of images and folders
ru.ContextMenu=Добавить «Открыть в Pressa» в контекстное меню изображений и папок
en.SendTo=Add Pressa to "Send to" (works with many selected files)
ru.SendTo=Добавить Pressa в меню «Отправить» (для нескольких файлов)
en.AddToPath=Add the pressa-cli command to PATH
ru.AddToPath=Добавить команду pressa-cli в PATH

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "contextmenu"; Description: "{cm:ContextMenu}"; GroupDescription: "{cm:Integration}"
Name: "sendto"; Description: "{cm:SendTo}"; GroupDescription: "{cm:Integration}"
Name: "addtopath"; Description: "{cm:AddToPath}"; GroupDescription: "{cm:Integration}"; Flags: unchecked

[Files]
Source: "{#Bin}\Pressa.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Bin}\pressa-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Pressa"; Filename: "{app}\Pressa.exe"
Name: "{autodesktop}\Pressa"; Filename: "{app}\Pressa.exe"; Tasks: desktopicon
Name: "{usersendto}\Pressa"; Filename: "{app}\Pressa.exe"; Tasks: sendto

[Registry]
; Right-click menu for any image type Windows knows, and for folders.
Root: HKCU; Subkey: "Software\Classes\SystemFileAssociations\image\shell\Pressa"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWith}"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\SystemFileAssociations\image\shell\Pressa"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\Pressa.exe"""; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\SystemFileAssociations\image\shell\Pressa\command"; ValueType: string; ValueName: ""; ValueData: """{app}\Pressa.exe"" ""%1"""; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Pressa"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWith}"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Pressa"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\Pressa.exe"""; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Pressa\command"; ValueType: string; ValueName: ""; ValueData: """{app}\Pressa.exe"" ""%1"""; Tasks: contextmenu
; Shows Pressa in "Open with" for images.
Root: HKCU; Subkey: "Software\Classes\Applications\Pressa.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "Pressa"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Applications\Pressa.exe\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\Pressa.exe"" ""%1"""

[Run]
Filename: "{app}\Pressa.exe"; Description: "{cm:LaunchProgram,Pressa}"; Flags: nowait postinstall skipifsilent

[Code]
const
  EnvKey = 'Environment';

function PathContains(Paths, Dir: string): Boolean;
begin
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') > 0;
end;

procedure AddToUserPath(Dir: string);
var
  Paths: string;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', Paths) then
    Paths := '';
  if PathContains(Paths, Dir) then
    exit;
  if (Paths <> '') and (Copy(Paths, Length(Paths), 1) <> ';') then
    Paths := Paths + ';';
  RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Paths + Dir);
end;

procedure RemoveFromUserPath(Dir: string);
var
  Paths: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', Paths) then
    exit;
  Paths := ';' + Paths + ';';
  P := Pos(';' + Uppercase(Dir) + ';', Uppercase(Paths));
  if P = 0 then
    exit;
  Delete(Paths, P, Length(Dir) + 1);
  Paths := Copy(Paths, 2, Length(Paths) - 2);
  RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Paths);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') then
    AddToUserPath(ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromUserPath(ExpandConstant('{app}'));
end;
