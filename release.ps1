<#
.SYNOPSIS
  Publishes a new Tempo version. Installed copies update themselves within a few hours.

.EXAMPLE
  .\release.ps1 1.1.0 "Added compact mode and fixed the image grid"

  1. Sets the version in Tempo.csproj
  2. Commits all changes and tags them v1.1.0
  3. Pushes to GitHub, where the "Release" workflow builds Tempo.exe and publishes the release
#>
param(
    [Parameter(Mandatory)][string]$Version,
    [string]$Notes = "",
    [switch]$Wait   # wait for GitHub to finish building and show the result
)
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3" }
$tag = "v$Version"
if (git tag --list $tag) { throw "$tag already exists. Pick a higher version." }

$current = ([xml](Get-Content Tempo.csproj)).Project.PropertyGroup.Version | Where-Object { $_ } | Select-Object -First 1
if ([version]$Version -le [version]$current) { throw "New version $Version must be higher than $current." }

# 1. Version number
(Get-Content Tempo.csproj -Raw) -replace '<Version>[^<]*</Version>', "<Version>$Version</Version>" |
    Set-Content Tempo.csproj -NoNewline

# 2. Make sure it still builds before publishing anything
dotnet build -c Release -nologo -v q
if ($LASTEXITCODE -ne 0) { throw "Build failed - nothing was published." }

# 3. Commit, tag (the tag message becomes the release notes) and push
if (-not $Notes) { $Notes = "Tempo $Version" }
git add -A
git commit -m "Release $tag" -m $Notes
git tag -a $tag -m $Notes
git push
git push origin $tag

Write-Host ""
Write-Host "Pushed $tag. GitHub is building the release:" -ForegroundColor Green
Write-Host "  https://github.com/JustSad42/Tempo/actions"
if ($Wait) {
    Start-Sleep -Seconds 5
    gh run watch (gh run list --workflow release.yml --limit 1 --json databaseId -q ".[0].databaseId") --exit-status
    gh release view $tag
}
