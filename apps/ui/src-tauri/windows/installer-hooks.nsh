; Beacon / Pulse Custom NSIS Installer Hooks
; Handles process termination, file synchronization, and Windows Firewall configurations.

!macro NSIS_HOOK_PREINSTALL
  DetailPrint "Stopping existing background processes..."
  nsExec::Exec 'taskkill /F /IM beacon-watchdog.exe'
  nsExec::Exec 'taskkill /F /IM beacon.exe'
  nsExec::Exec 'taskkill /F /IM pulse.exe'
  nsExec::Exec 'taskkill /F /IM Beacon.exe'
  nsExec::Exec 'taskkill /F /IM Pulse.exe'
  
  ; Delete old firewall rules to ensure clean setup
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Beacon-UDP-Stream"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Beacon-TCP-Control"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Pulse-UDP-ClientRecv"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Beacon-Pulse-UDP-Discovery"'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Synchronizing background services next to the UI executable..."
  ; Copy service and watchdog binaries from the resources folder to $INSTDIR
  IfFileExists "$INSTDIR\resources\beacon.exe" 0 +2
    CopyFiles "$INSTDIR\resources\beacon.exe" "$INSTDIR\beacon.exe"
  IfFileExists "$INSTDIR\resources\pulse.exe" 0 +2
    CopyFiles "$INSTDIR\resources\pulse.exe" "$INSTDIR\pulse.exe"
  IfFileExists "$INSTDIR\resources\beacon-watchdog.exe" 0 +2
    CopyFiles "$INSTDIR\resources\beacon-watchdog.exe" "$INSTDIR\beacon-watchdog.exe"

  DetailPrint "Configuring Windows Firewall rules for LAN streaming..."
  nsExec::Exec 'netsh advfirewall firewall add rule name="Beacon-UDP-Stream" dir=in action=allow protocol=UDP localport=45100 enable=yes profile=any'
  nsExec::Exec 'netsh advfirewall firewall add rule name="Beacon-TCP-Control" dir=in action=allow protocol=TCP localport=45101 enable=yes profile=any'
  nsExec::Exec 'netsh advfirewall firewall add rule name="Pulse-UDP-ClientRecv" dir=in action=allow protocol=UDP localport=45102 enable=yes profile=any'
  nsExec::Exec 'netsh advfirewall firewall add rule name="Beacon-Pulse-UDP-Discovery" dir=in action=allow protocol=UDP localport=45199 enable=yes profile=any'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "Stopping existing background processes for uninstallation..."
  nsExec::Exec 'taskkill /F /IM beacon-watchdog.exe'
  nsExec::Exec 'taskkill /F /IM beacon.exe'
  nsExec::Exec 'taskkill /F /IM pulse.exe'
  nsExec::Exec 'taskkill /F /IM Beacon.exe'
  nsExec::Exec 'taskkill /F /IM Pulse.exe'

  ; Delete custom files copied during post-install
  Delete "$INSTDIR\beacon.exe"
  Delete "$INSTDIR\pulse.exe"
  Delete "$INSTDIR\beacon-watchdog.exe"

  DetailPrint "Deleting Windows Firewall rules..."
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Beacon-UDP-Stream"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Beacon-TCP-Control"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Pulse-UDP-ClientRecv"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Beacon-Pulse-UDP-Discovery"'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Nothing additional required
!macroend
