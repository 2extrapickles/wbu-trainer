# Builds the trainer (64-bit exe) and the in-game helper (32-bit dinput8.dll) and packages both
# as dist\wbu-trainer-<version>.zip
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

# Release builds must not embed local paths (user folder, cargo registry, project location) in the
# binaries' panic/debug strings: remap them to neutral prefixes.
$rustup = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }
$cargo = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$remaps = @("$cargo=cargo", "$rustup=rustup", "$PSScriptRoot=wbu-trainer", "$env:USERPROFILE=home")
# Encoded form (0x1f-separated) because paths may contain spaces.
$env:CARGO_ENCODED_RUSTFLAGS = ($remaps | ForEach-Object { "--remap-path-prefix=$_" }) -join [char]0x1f

Push-Location helper; cargo build --release; $h = $LASTEXITCODE; Pop-Location
if ($h -ne 0) { throw 'helper build failed' }
Push-Location trainer; cargo build --release --target-dir target\pkg; $t = $LASTEXITCODE; Pop-Location
if ($t -ne 0) { throw 'trainer build failed' }

$version = (Select-String -Path trainer\Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$stage = Join-Path 'dist' "wbu-trainer-$version"
New-Item -ItemType Directory -Force $stage | Out-Null

Copy-Item 'trainer\target\pkg\release\wbu-trainer.exe' $stage -Force
Copy-Item 'helper\target\i686-pc-windows-msvc\release\dinput8.dll' $stage -Force
Copy-Item 'dist-readme.txt' (Join-Path $stage 'README.txt') -Force
Copy-Item 'LICENSE' (Join-Path $stage 'LICENSE.txt') -Force

$zip = "$stage.zip"
Compress-Archive -Path "$stage\*" -DestinationPath $zip -Force
Get-Item $zip | Select-Object Name, Length
