# Publica o Telinha no GitHub e dispara o build do instalador.
# Uso: clique direito > "Executar com o PowerShell", ou: powershell -ExecutionPolicy Bypass -File .\publicar.ps1
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot
$repo = 'BySzx/telinha-app'
$key = Join-Path (Split-Path $PSScriptRoot -Parent) 'telinha-updater.key'

function Fail($msg) { Write-Host "`n✖ $msg" -ForegroundColor Red; Read-Host 'Enter pra fechar'; exit 1 }
function Step($msg) { Write-Host "`n▸ $msg" -ForegroundColor Cyan }

foreach ($tool in @(@{ cmd = 'git'; id = 'Git.Git' }, @{ cmd = 'gh'; id = 'GitHub.cli' })) {
  if (-not (Get-Command $tool.cmd -ErrorAction SilentlyContinue)) {
    Step "Instalando $($tool.cmd)…"
    winget install --id $tool.id -e --accept-source-agreements --accept-package-agreements
    Fail "Instalei o $($tool.cmd). Feche este terminal, abra de novo e rode o script outra vez."
  }
}
if (-not (Test-Path $key)) { Fail "Não achei a chave em $key" }

if (Test-Path 'release.yml') {
  New-Item -ItemType Directory -Force '.github\workflows' | Out-Null
  Move-Item -Force 'release.yml' '.github\workflows\release.yml'
}

gh auth status *> $null
if ($LASTEXITCODE -ne 0) {
  Step 'Login no GitHub (abre o navegador)'
  gh auth login --web --git-protocol https
  if ($LASTEXITCODE -ne 0) { Fail 'Login no GitHub falhou.' }
}

Step 'Preparando o git'
if (-not (Test-Path '.git')) { git init -b main | Out-Null }
if (-not (git config user.name)) { git config user.name 'BySzx'; git config user.email 'BySzx@users.noreply.github.com' }
git add -A
git diff --cached --quiet
if ($LASTEXITCODE -ne 0) { git commit -m 'Telinha app' | Out-Null }

Step "Subindo pro github.com/$repo"
gh repo view $repo *> $null
if ($LASTEXITCODE -ne 0) {
  gh repo create $repo --public --source . --remote origin --push --description 'Compartilhar tela com a galera'
} else {
  if (-not (git remote)) { git remote add origin "https://github.com/$repo.git" }
  git push -u origin main
}
if ($LASTEXITCODE -ne 0) { Fail 'Push falhou.' }

Step 'Guardando a chave de atualização como secret'
Get-Content $key -Raw | gh secret set TAURI_SIGNING_PRIVATE_KEY -R $repo
if ($LASTEXITCODE -ne 0) { Fail 'Não deu pra salvar o secret.' }

$version = (Get-Content 'src-tauri\tauri.conf.json' -Raw | ConvertFrom-Json).version
Step "Criando a tag v$version (dispara o build)"
git tag "v$version" 2>$null
git push origin "v$version"
if ($LASTEXITCODE -ne 0) { Fail 'Push da tag falhou.' }

Write-Host "`n✔ Pronto! O instalador fica pronto em ~10 min em https://github.com/$repo/releases" -ForegroundColor Green
Write-Host "  Acompanhe: https://github.com/$repo/actions"
gh browse -R $repo --actions 2>$null
Read-Host 'Enter pra fechar'
