; Inno Setup 6 script — build with packaging\windows\build.ps1
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{8F3C2A51-6B7E-4C1D-9A2F-3E5B7C9D1A42}
AppName=AirLink
AppVersion={#AppVersion}
AppVerName=AirLink {#AppVersion}
AppPublisher=M0JSX
AppPublisherURL=https://github.com/M0JSX/airlink
DefaultDirName={autopf}\AirLink
DefaultGroupName=AirLink
DisableProgramGroupPage=yes
OutputDir=..\..\dist
OutputBaseFilename=AirLink-{#AppVersion}-windows-x64-setup
SetupIconFile=..\icons\airlink.ico
UninstallDisplayIcon={app}\airlink.ico
UninstallDisplayName=AirLink
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
PrivilegesRequired=admin
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "firewall"; Description: "Allow AirLink through Windows Firewall (needed to answer calls)"; GroupDescription: "Network:"

[Files]
Source: "..\..\target\release\airlink.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\dist\windows\*.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\icons\airlink.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\AirLink"; Filename: "{app}\airlink.exe"; IconFilename: "{app}\airlink.ico"
Name: "{autodesktop}\AirLink"; Filename: "{app}\airlink.exe"; IconFilename: "{app}\airlink.ico"; Tasks: desktopicon

[Run]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""AirLink"""; Flags: runhidden; Tasks: firewall
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""AirLink"" dir=in action=allow program=""{app}\airlink.exe"" enable=yes"; Flags: runhidden; Tasks: firewall
Filename: "{app}\airlink.exe"; Description: "{cm:LaunchProgram,AirLink}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""AirLink"""; Flags: runhidden; RunOnceId: "DelFirewallRule"
