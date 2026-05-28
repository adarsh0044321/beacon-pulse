; LANShare Custom NSIS Installer Hooks
; Handles process termination, file synchronization, and Windows Firewall configurations.

!macro NSIS_HOOK_PREINSTALL
  DetailPrint "LANShare Setup: Stopping existing background processes..."
  nsExec::Exec 'taskkill /F /IM lanshare-watchdog.exe'
  nsExec::Exec 'taskkill /F /IM lanshare-service.exe'
  nsExec::Exec 'taskkill /F /IM lanshare-host.exe'
  nsExec::Exec 'taskkill /F /IM lanshare-player.exe'
  nsExec::Exec 'taskkill /F /IM lanshare-ui.exe'
  nsExec::Exec 'taskkill /F /IM LANShare.exe'
  nsExec::Exec 'taskkill /F /IM LANShareHost.exe'
  nsExec::Exec 'taskkill /F /IM LANSharePlayer.exe'
  
  ; Delete old firewall rules to ensure clean setup
  nsExec::Exec 'netsh advfirewall firewall delete rule name="LANShare-UDP-Stream"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="LANShare-TCP-Control"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="LANShare-UDP-Client"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="LANShare-UDP-Discovery"'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "LANShare Setup: Synchronizing background services next to the UI executable..."
  ; Copy service and watchdog binaries from the resources folder to $INSTDIR
  CopyFiles "$INSTDIR\resources\lanshare-host.exe" "$INSTDIR\lanshare-host.exe"
  CopyFiles "$INSTDIR\resources\lanshare-player.exe" "$INSTDIR\lanshare-player.exe"
  CopyFiles "$INSTDIR\resources\lanshare-watchdog.exe" "$INSTDIR\lanshare-watchdog.exe"

  DetailPrint "LANShare Setup: Configuring Windows Firewall rules for LAN streaming..."
  nsExec::Exec 'netsh advfirewall firewall add rule name="LANShare-UDP-Stream" dir=in action=allow protocol=UDP localport=45100 enable=yes profile=any'
  nsExec::Exec 'netsh advfirewall firewall add rule name="LANShare-TCP-Control" dir=in action=allow protocol=TCP localport=45101 enable=yes profile=any'
  nsExec::Exec 'netsh advfirewall firewall add rule name="LANShare-UDP-Client" dir=in action=allow protocol=UDP localport=45102 enable=yes profile=any'
  nsExec::Exec 'netsh advfirewall firewall add rule name="LANShare-UDP-Discovery" dir=in action=allow protocol=UDP localport=45199 enable=yes profile=any'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "LANShare Setup: Stopping existing background processes for uninstallation..."
  nsExec::Exec 'taskkill /F /IM lanshare-watchdog.exe'
  nsExec::Exec 'taskkill /F /IM lanshare-service.exe'
  nsExec::Exec 'taskkill /F /IM lanshare-host.exe'
  nsExec::Exec 'taskkill /F /IM lanshare-player.exe'
  nsExec::Exec 'taskkill /F /IM lanshare-ui.exe'
  nsExec::Exec 'taskkill /F /IM LANShare.exe'
  nsExec::Exec 'taskkill /F /IM LANShareHost.exe'
  nsExec::Exec 'taskkill /F /IM LANSharePlayer.exe'

  ; Delete custom files copied during post-install
  Delete "$INSTDIR\lanshare-service.exe"
  Delete "$INSTDIR\lanshare-host.exe"
  Delete "$INSTDIR\lanshare-player.exe"
  Delete "$INSTDIR\lanshare-watchdog.exe"

  DetailPrint "LANShare Setup: Deleting Windows Firewall rules..."
  nsExec::Exec 'netsh advfirewall firewall delete rule name="LANShare-UDP-Stream"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="LANShare-TCP-Control"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="LANShare-UDP-Client"'
  nsExec::Exec 'netsh advfirewall firewall delete rule name="LANShare-UDP-Discovery"'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Nothing additional required
!macroend
