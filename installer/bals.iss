; Instalador liviano de Bals (sin permisos de administrador)
; Compilado automáticamente en GitHub Actions con Inno Setup 6 (`iscc /DMyAppVersion=X.Y.Z installer\bals.iss`)

#ifndef MyAppVersion
  #define MyAppVersion "0.2.0"
#endif

[Setup]
AppId={{8E92D6B4-4F1D-4C2B-9A7B-5C31B2E0D4B1}
AppName=Bals
AppVersion={#MyAppVersion}
AppPublisher=Santiago Conelli
AppPublisherURL=https://github.com/sconelli24/bals
AppSupportURL=https://github.com/sconelli24/bals/issues
DefaultDirName={localappdata}\Programs\Bals
DefaultGroupName=Bals
AllowNoIcons=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
SetupIconFile=..\assets\bals.ico
UninstallDisplayIcon={app}\bals.exe
OutputDir=..\dist
OutputBaseFilename=Bals-Setup-v{#MyAppVersion}
CloseApplications=force

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "autostart"; Description: "Iniciar Bals automáticamente con Windows (en la bandeja del sistema)"; GroupDescription: "Opciones de inicio:"

[Files]
Source: "..\target\release\bals.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Bals"; Filename: "{app}\bals.exe"; IconFilename: "{app}\bals.exe"
Name: "{group}\Desinstalar Bals"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Bals"; Filename: "{app}\bals.exe"; IconFilename: "{app}\bals.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Bals"; ValueData: """{app}\bals.exe"" --tray"; Flags: uninsdeletevalue; Tasks: autostart

[Run]
Filename: "{app}\bals.exe"; Description: "Abrir Bals ahora"; Flags: nowait postinstall skipifsilent
