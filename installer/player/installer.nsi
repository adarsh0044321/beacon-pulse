; Pulse Player — Standalone NSIS Installer
; Installs just pulse.exe with Start Menu + Desktop shortcuts

!include "MUI2.nsh"

; ── Metadata ────────────────────────────────────────────────────────────
Name "Pulse Player"
OutFile "PulsePlayer-Setup.exe"
InstallDir "$LOCALAPPDATA\PulsePlayer"
InstallDirRegKey HKCU "Software\Pulse" "InstallDir"
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
    File "pulse.exe"
    
    ; Write registry for uninstall
    WriteRegStr HKCU "Software\Pulse" "InstallDir" "$INSTDIR"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\PulsePlayer" \
                     "DisplayName" "Pulse Player"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\PulsePlayer" \
                     "UninstallString" '"$INSTDIR\Uninstall.exe"'
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\PulsePlayer" \
                     "DisplayIcon" "$INSTDIR\pulse.exe"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\PulsePlayer" \
                     "Publisher" "Pulse"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\PulsePlayer" \
                     "DisplayVersion" "1.0.0"
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\PulsePlayer" \
                       "NoModify" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\PulsePlayer" \
                       "NoRepair" 1
    
    ; Create uninstaller
    WriteUninstaller "$INSTDIR\Uninstall.exe"
    
    ; Desktop shortcut
    CreateShortcut "$DESKTOP\Pulse Player.lnk" "$INSTDIR\pulse.exe" "" \
                   "$INSTDIR\pulse.exe" 0
    
    ; Start Menu
    CreateDirectory "$SMPROGRAMS\Pulse"
    CreateShortcut "$SMPROGRAMS\Pulse\Pulse Player.lnk" "$INSTDIR\pulse.exe" "" \
                   "$INSTDIR\pulse.exe" 0
    CreateShortcut "$SMPROGRAMS\Pulse\Uninstall Player.lnk" "$INSTDIR\Uninstall.exe"
SectionEnd

; ── Uninstall Section ───────────────────────────────────────────────────
Section "Uninstall"
    Delete "$INSTDIR\pulse.exe"
    Delete "$INSTDIR\Uninstall.exe"
    RMDir "$INSTDIR"
    
    Delete "$DESKTOP\Pulse Player.lnk"
    Delete "$SMPROGRAMS\Pulse\Pulse Player.lnk"
    Delete "$SMPROGRAMS\Pulse\Uninstall Player.lnk"
    RMDir "$SMPROGRAMS\Pulse"
    
    DeleteRegKey HKCU "Software\Pulse"
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\PulsePlayer"
SectionEnd
