# Downloads a static FFmpeg build into target\ffmpeg\ffmpeg.exe for bundling with Pressa.
# Source: https://github.com/FFmpeg-Builds/FFmpeg-Builds (GPL build: x264, x265, SVT-AV1, NVENC, QSV, AMF).
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'  # Invoke-WebRequest is very slow with the progress bar
$root = Resolve-Path "$PSScriptRoot\..\.."
$dest = Join-Path $root 'target\ffmpeg'
$exe = Join-Path $dest 'ffmpeg.exe'
if (Test-Path $exe) { Write-Host "FFmpeg already present: $exe"; return }

$name = 'ffmpeg-n9.0-latest-win64-gpl-9.0'
$url = "https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases/download/latest/$name.zip"
New-Item -ItemType Directory -Force $dest | Out-Null
$zip = Join-Path $dest "$name.zip"
Write-Host "Downloading $url"
Invoke-WebRequest $url -OutFile $zip

Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::OpenRead($zip)
try {
    $entry = $archive.Entries | Where-Object { $_.FullName -like '*/bin/ffmpeg.exe' } | Select-Object -First 1
    if (-not $entry) { throw 'ffmpeg.exe not found in archive' }
    [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $exe, $true)
    $license = $archive.Entries | Where-Object { $_.FullName -like '*/LICENSE.txt' } | Select-Object -First 1
    if ($license) { [System.IO.Compression.ZipFileExtensions]::ExtractToFile($license, (Join-Path $dest 'FFMPEG-LICENSE.txt'), $true) }
}
finally {
    $archive.Dispose()
}
Remove-Item $zip
Write-Host "FFmpeg ready: $exe"
