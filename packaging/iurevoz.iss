; Instalador de IureVoz para Windows (Inno Setup 6). Lo arma el flujo de GitHub Actions:
;   iscc /DVersion=X.Y.Z packaging\iurevoz.iss
; Se instala para el usuario (sin permisos de administrador). Los modelos se descargan
; al primer uso en %LOCALAPPDATA%\iurevoz.

#ifndef Version
  #define Version "0.0.0"
#endif

[Setup]
; iureapps busca la clave de desinstalación por el nombre de la app («IureVoz_is1»)
AppId=IureVoz
AppName=IureVoz
AppVersion={#Version}
AppPublisher=Iurefficient
AppPublisherURL=https://github.com/ellaguno/iurevoz
DefaultDirName={localappdata}\Programs\IureVoz
DefaultGroupName=IureVoz
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\target\instalador
OutputBaseFilename=IureVoz-{#Version}-windows-x64
Compression=lzma2
SolidCompression=yes
UninstallDisplayIcon={app}\iurevoz.exe
SetupIconFile=icono\iurevoz.ico
CloseApplications=yes

[Languages]
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"

[Tasks]
Name: "inicio"; Description: "Abrir IureVoz al iniciar la sesión (solo el ícono junto al reloj)"; Flags: unchecked

[Files]
Source: "..\target\release\iurevoz.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; el AppUserModelID deja que las notificaciones digan «IureVoz»
Name: "{userprograms}\IureVoz"; Filename: "{app}\iurevoz.exe"; AppUserModelID: "mx.iurefficient.IureVoz"

[Registry]
; el mismo valor que pone Ajustes → Inicio
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "IureVoz"; ValueData: """{app}\iurevoz.exe"" --autoarranque"; Flags: uninsdeletevalue; Tasks: inicio

[Run]
Filename: "{app}\iurevoz.exe"; Description: "Abrir IureVoz"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{cmd}"; Parameters: "/c reg delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v IureVoz /f"; Flags: runhidden; RunOnceId: "QuitarInicio"
