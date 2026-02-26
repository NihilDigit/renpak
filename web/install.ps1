# renpak installer for Windows
# Usage: irm https://renpak.vercel.app/install.ps1 | iex
$ErrorActionPreference = "Stop"

$repo = "NihilDigit/renpak"
$installDir = if ($env:RENPAK_INSTALL_DIR) { $env:RENPAK_INSTALL_DIR } else { "$env:USERPROFILE\.local\bin" }

# Get latest release
$release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest"
$tag = $release.tag_name
$url = "https://github.com/$repo/releases/download/$tag/renpak-windows-x86_64.zip"

Write-Host "renpak $tag (windows-x86_64)"
Write-Host ""

# Download and extract
New-Item -ItemType Directory -Force -Path $installDir | Out-Null
$tmp = "$env:TEMP\renpak-$tag.zip"
Write-Host "Downloading $url"
Invoke-WebRequest -Uri $url -OutFile $tmp
Expand-Archive -Path $tmp -DestinationPath $installDir -Force
Remove-Item $tmp

Write-Host ""
Write-Host "Installed to $installDir\renpak.exe"

# Check PATH
if ($env:PATH -notlike "*$installDir*") {
    Write-Host ""
    Write-Host "Adding $installDir to user PATH..."
    $userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($userPath -notlike "*$installDir*") {
        [Environment]::SetEnvironmentVariable("PATH", "$userPath;$installDir", "User")
        $env:PATH = "$env:PATH;$installDir"
        Write-Host "Done. Restart your terminal for PATH changes to take effect."
    }
}

Write-Host ""
Write-Host "Run 'renpak' in a Ren'Py game directory to start."
