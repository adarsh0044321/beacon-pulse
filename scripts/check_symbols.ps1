param(
    [string]$Path = "target/debug"
)

$hostExe = Join-Path $Path "beacon.exe"
$playerExe = Join-Path $Path "pulse.exe"

$success = $true

Write-Host "Verifying symbols for $hostExe..."
if (Test-Path $hostExe) {
    $bytes = [System.IO.File]::ReadAllBytes($hostExe)
    $ascii = [System.Text.Encoding]::ASCII.GetString($bytes)
    $unicode = [System.Text.Encoding]::Unicode.GetString($bytes)
    if ($ascii.Contains("StretchDIBits") -or $unicode.Contains("StretchDIBits")) {
        Write-Warning "Host contains StretchDIBits (GDI Painting Loop)!"
        $success = $false
    } else {
        Write-Host "Host check passed: No StretchDIBits."
    }
} else {
    Write-Warning "Host executable not found at $hostExe"
    $success = $false
}

Write-Host "Verifying symbols for $playerExe..."
if (Test-Path $playerExe) {
    $bytes = [System.IO.File]::ReadAllBytes($playerExe)
    $ascii = [System.Text.Encoding]::ASCII.GetString($bytes)
    $unicode = [System.Text.Encoding]::Unicode.GetString($bytes)
    
    $hasWGC = $ascii.Contains("Direct3D11CaptureFramePool") -or 
              $unicode.Contains("Direct3D11CaptureFramePool") -or 
              $ascii.Contains("GraphicsCaptureSession") -or 
              $unicode.Contains("GraphicsCaptureSession")
              
    if ($hasWGC) {
        Write-Warning "Player contains WGC/GraphicsCapture symbols!"
        $success = $false
    } else {
        Write-Host "Player check passed: No WGC symbols."
    }
} else {
    Write-Warning "Player executable not found at $playerExe"
    $success = $false
}

if (-not $success) {
    Write-Error "Verification FAILED!"
    exit 1
} else {
    Write-Host "Verification PASSED!"
}
