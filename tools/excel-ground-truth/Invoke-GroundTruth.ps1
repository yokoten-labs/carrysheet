#Requires -Version 5.1
<#
.SYNOPSIS
One command: build the workbooks with Excel, then export what Excel computed.

.DESCRIPTION
Runs Build-Workbooks.ps1 and then Export-GroundTruth.ps1 with the same folders.
Use -WhatIf to see what would happen without starting Excel or writing files.

.EXAMPLE
powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\excel-ground-truth\Invoke-GroundTruth.ps1

.EXAMPLE
powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\excel-ground-truth\Invoke-GroundTruth.ps1 -Topic dates/leap-1900
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string]$CasesPath,
    [string]$WorkbookPath,
    [string]$OutputPath,
    [string[]]$Topic
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$buildParams = @{}
$exportParams = @{}
if ($CasesPath) { $buildParams.CasesPath = $CasesPath }
if ($WorkbookPath) { $buildParams.WorkbookPath = $WorkbookPath; $exportParams.WorkbookPath = $WorkbookPath }
if ($OutputPath) { $exportParams.OutputPath = $OutputPath }
if ($Topic) { $buildParams.Topic = $Topic }

$global:LASTEXITCODE = 0
& (Join-Path $PSScriptRoot 'Build-Workbooks.ps1') @buildParams -WhatIf:$WhatIfPreference
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host ''
& (Join-Path $PSScriptRoot 'Export-GroundTruth.ps1') @exportParams -WhatIf:$WhatIfPreference
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
