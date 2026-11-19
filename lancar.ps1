# Lança uma versão nova do Telinha: sobe o código, cria a tag e o GitHub gera o instalador.
# Uso: .\lancar.ps1 0.1.1
param([Parameter(Mandatory = $true)][string]$Versao)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot

function Fail($msg) { Write-Host "`n✖ $msg" -ForegroundColor Red; exit 1 }
if ($Versao -notmatch '^\d+\.\d+\.\d+$') { Fail 'Versão no formato 1.2.3' }

New-Item -ItemType Directory -Force '.github\workflows' | Out-Null
if (Test-Path 'release.yml') { Move-Item -Force 'release.yml' '.github\workflows\release.yml' }
if (Test-Path 'sfu-release.yml') { Move-Item -Force 'sfu-release.yml' '.github\workflows\sfu.yml' }

$utf8 = New-Object System.Text.UTF8Encoding($false)
foreach ($file in 'src-tauri\tauri.conf.json', 'package.json') {
  $text = [IO.File]::ReadAllText((Resolve-Path $file))
  $text = [regex]::Replace($text, '"version": "[^"]+"', "`"version`": `"$Versao`"", 1)
  [IO.File]::WriteAllText((Resolve-Path $file), $text, $utf8)
}
$cargo = [IO.File]::ReadAllText((Resolve-Path 'src-tauri\Cargo.toml'))
$cargo = [regex]::Replace($cargo, '(?m)^version = "[^"]+"', "version = `"$Versao`"", 1)
[IO.File]::WriteAllText((Resolve-Path 'src-tauri\Cargo.toml'), $cargo, $utf8)

git add -A
git commit -m "v$Versao" | Out-Null
git push
if ($LASTEXITCODE -ne 0) { Fail 'Push falhou.' }
git tag "v$Versao"
git push origin "v$Versao"
if ($LASTEXITCODE -ne 0) { Fail 'Push da tag falhou.' }
Write-Host "`n✔ v$Versao enviada. Instalador em ~5 min: https://github.com/BySzx/telinha-app/releases" -ForegroundColor Green
