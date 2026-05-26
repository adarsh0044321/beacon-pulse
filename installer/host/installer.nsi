; Beacon Host — Standalone NSIS Installer
; Installs just beacon.exe with Start Menu + Desktop shortcuts

!include "MUI2.nsh"

; ── Metadata ────────────────────────────────────────────────────────────
Name "Beacon Host"
OutFile "BeaconHost-Setup.exe"
InstallDir "$LOCALAPPDATA\BeaconHost"
InstallDirRegKey HKCU "Software\Beacon" "InstallDir"
RequestExecutionLevel user

; ── UI ──────────────────────────────────────────────────────────────────
!define MUI_ICON "icon.ico"
!define MUI_UNICON "icon.ico"
!define MUI_ABORTWARNING

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; ── Install Section ─────────────────────────────────────────────────────
Section "Install"
    SetOutPath "$INSTDIR"
    
    ; Main binary
    File "beacon.exe"
    File "beacon-watchdog.exe"
    
    ; Write registry for uninstall
    WriteRegStr HKCU "Software\Beacon" "InstallDir" "$INSTDIR"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\BeaconHost" \
                     "DisplayName" "Beacon Host"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\BeaconHost" \
                     "UninstallString" '"$INSTDIR\Uninstall.exe"'
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\BeaconHost" \
                     "DisplayIcon" "$INSTDIR\beacon.exe"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\BeaconHost" \
                     "Publisher" "Beacon"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\BeaconHost" \
                     "DisplayVersion" "1.0.0"
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\BeaconHost" \
                       "NoModify" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\BeaconHost" \
                       "NoRepair" 1
    
    ; Create uninstaller
    WriteUninstaller "$INSTDIR\Uninstall.exe"
    
    ; Desktop shortcut
    CreateShortcut "$DESKTOP\Beacon Host.lnk" "$INSTDIR\beacon.exe" "" \
                   "$INSTDIR\beacon.exe" 0
    
    ; Start Menu
    CreateDirectory "$SMPROGRAMS\Beacon"
    CreateShortcut "$SMPROGRAMS\Beacon\Beacon Host.lnk" "$INSTDIR\beacon.exe" "" \
                   "$INSTDIR\beacon.exe" 0
    CreateShortcut "$SMPROGRAMS\Beacon\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
SectionEnd

; ── Uninstall Section ───────────────────────────────────────────────────
Section "Uninstall"
    Delete "$INSTDIR\beacon.exe"
    Delete "$INSTDIR\beacon-watchdog.exe"
    Delete "$INSTDIR\Uninstall.exe"
    RMDir "$INSTDIR"
    
    Delete "$DESKTOP\Beacon Host.lnk"
    Delete "$SMPROGRAMS\Beacon\Beacon Host.lnk"
    Delete "$SMPROGRAMS\Beacon\Uninstall.lnk"
    RMDir "$SMPROGRAMS\Beacon"
    
    DeleteRegKey HKCU "Software\Beacon"
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\BeaconHost"
SectionEnd
