#Requires -Version 5.1
<#
.SYNOPSIS
Opens the built workbooks in Excel, recalculates them fully, and writes what
Excel computed to JSON.

.DESCRIPTION
Reads manifest.json from -WorkbookPath (written by Build-Workbooks.ps1), opens
each listed workbook read-only in a separate hidden Excel, runs a full
recalculation (CalculateFullRebuild), and writes one JSON file per topic to
-OutputPath: every formula cell's Value2, its type, the displayed text and the
number format, plus the Excel version, locale and date system.

The JSON is UTF-8 without BOM, LF line ends, keys sorted. User names, the
computer name and home paths are replaced by <user>, <computer> and ~.

Only files listed in the manifest and inside -WorkbookPath are opened, and only
.json files inside -OutputPath are written. The workbooks are not saved.

.PARAMETER WorkbookPath
Folder with manifest.json and the workbooks. Default: out\workbooks next to this script.

.PARAMETER OutputPath
Folder for the JSON results. Default: the expected folder next to this script.

.EXAMPLE
powershell -NoProfile -ExecutionPolicy Bypass -File .\Export-GroundTruth.ps1 -WhatIf
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string]$WorkbookPath,
    [string]$OutputPath
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'GroundTruth.psm1') -Force

$MaxSpillCells = 1000

function Add-CellResult {
    # Reads one declared cell, and when it spills, every cell it spills into.
    param($Cell, $Spec, [bool]$DynamicArrays, $Cells)
    $address = Get-CellAddress $Cell
    $entry = Read-ExcelCell -Cell $Cell -DynamicArrays $DynamicArrays
    $entry.set = [string]$Spec.set
    $Cells[$address] = $entry
    if (-not $DynamicArrays) { return }
    $hasSpill = $false
    try { $hasSpill = [bool]$Cell.HasSpill } catch { $hasSpill = $false }
    if (-not $hasSpill) { return }

    $spill = $Cell.SpillingToRange
    $entry.spill = Get-CellAddress $spill
    $rows = [int]$spill.Rows.Count
    $cols = [int]$spill.Columns.Count
    $read = 0
    for ($r = 1; $r -le $rows; $r++) {
        for ($c = 1; $c -le $cols; $c++) {
            if ($r -eq 1 -and $c -eq 1) { continue }
            if ($read -ge $MaxSpillCells) { $entry.spillTruncated = $true; break }
            $child = $spill.Cells.Item($r, $c)
            $childAddress = Get-CellAddress $child
            if (-not $Cells.Contains($childAddress)) {
                $childEntry = Read-ExcelCell -Cell $child -DynamicArrays $DynamicArrays
                $childEntry.spilledFrom = $address
                $Cells[$childAddress] = $childEntry
            }
            Remove-ComObject $child
            $read++
        }
    }
    Remove-ComObject $spill
}

function Read-WorkbookCases {
    # Returns an ordered map: case id -> result. Adds expectation differences to $Differences.
    param($Workbook, $Record, [bool]$DynamicArrays, [System.Collections.ArrayList]$Differences, [ref]$CellCount)
    $results = [ordered]@{}
    foreach ($case in @(Get-JsonProperty $Record 'cases' @())) {
        $id = [string]$case.id
        $status = [string]$case.status
        $out = [ordered]@{
            sheet    = [string]$case.sheet
            status   = $status
            volatile = [bool](Get-JsonProperty $case 'volatile' $false)
        }
        if ($status -ne 'built') {
            $out.reason = [string](Get-JsonProperty $case 'reason' '')
            $results[$id] = $out
            continue
        }
        $sheet = $Workbook.Worksheets.Item($out.sheet)
        $cells = [ordered]@{}
        foreach ($spec in @(Get-JsonProperty $case 'cells' @())) {
            $range = $sheet.Range([string]$spec.address)
            $rows = [int]$range.Rows.Count
            $cols = [int]$range.Columns.Count
            for ($r = 1; $r -le $rows; $r++) {
                for ($c = 1; $c -le $cols; $c++) {
                    $cell = $range.Cells.Item($r, $c)
                    Add-CellResult -Cell $cell -Spec $spec -DynamicArrays $DynamicArrays -Cells $cells
                    Remove-ComObject $cell
                }
            }
            Remove-ComObject $range
        }
        Remove-ComObject $sheet
        $out.cells = $cells
        $CellCount.Value += $cells.Count

        $expect = Get-JsonProperty $case 'expect'
        if ($null -ne $expect -and -not $out.volatile) {
            foreach ($p in $expect.PSObject.Properties) {
                $expected = $p.Value
                if (-not ($expected -is [bool] -or $expected -is [string])) { $expected = [double]$expected }
                $actual = $null
                if ($cells.Contains($p.Name)) { $actual = $cells[$p.Name] }
                if (-not (Test-Expectation -Expected $expected -Entry $actual)) {
                    $got = $(if ($null -eq $actual) { 'nothing' } else { '{0} ({1})' -f $actual.value, $actual.type })
                    [void]$Differences.Add(('{0} {1}: case note expects {2}, Excel gave {3}' -f $id, $p.Name, $expected, $got))
                }
            }
        }
        $results[$id] = $out
    }
    return $results
}

try {
    if (-not $WorkbookPath) { $WorkbookPath = Join-Path (Join-Path $PSScriptRoot 'out') 'workbooks' }
    if (-not $OutputPath) { $OutputPath = Join-Path $PSScriptRoot 'expected' }
    $workbookFull = $PSCmdlet.SessionState.Path.GetUnresolvedProviderPathFromPSPath($WorkbookPath)
    $outputFull = $PSCmdlet.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
    $outputFull = Assert-WorkFolder -Path $outputFull -AllowedExtensions @('.json') -Purpose 'output'

    $manifestPath = Join-Path $workbookFull 'manifest.json'
    if (-not [System.IO.File]::Exists($manifestPath)) {
        if ($WhatIfPreference) {
            Write-Host 'What if: would read manifest.json from the workbook folder (it does not exist yet; Build-Workbooks.ps1 writes it).'
            return
        }
        throw 'manifest.json is missing from the workbook folder. Run Build-Workbooks.ps1 first (or Invoke-GroundTruth.ps1, which runs both).'
    }
    $manifest = ConvertFrom-Json -InputObject (Read-Utf8Text $manifestPath)
    if ((Get-JsonProperty $manifest 'schema') -ne 1) { throw 'manifest.json has an unknown schema. Rebuild the workbooks with Build-Workbooks.ps1.' }

    # Group the workbook records by topic, keeping manifest order.
    $topics = [ordered]@{}
    foreach ($record in @(Get-JsonProperty $manifest 'workbooks' @())) {
        $topic = [string]$record.topic
        $full = Resolve-InsideFolder -Root $workbookFull -Relative ([string]$record.file)
        if (-not [System.IO.File]::Exists($full)) { throw "The manifest lists $($record.file), but that file is missing. Rebuild the workbooks." }
        if (-not $topics.Contains($topic)) { $topics[$topic] = New-Object System.Collections.ArrayList }
        [void]$topics[$topic].Add([pscustomobject]@{ Record = $record; Path = $full })
    }

    $plan = New-Object System.Collections.ArrayList
    foreach ($topic in $topics.Keys) {
        $target = Resolve-InsideFolder -Root $outputFull -Relative ($topic + '.json')
        if ($PSCmdlet.ShouldProcess("$topic.json", 'Open the workbooks in Excel and write results')) {
            [void]$plan.Add([pscustomobject]@{ Topic = $topic; Target = $target; Books = $topics[$topic] })
        }
    }
    if ($plan.Count -eq 0) {
        Write-Host 'Nothing to export.'
        return
    }

    Assert-ExcelIdle
    $session = $null
    $started = Get-Date
    $differences = New-Object System.Collections.ArrayList
    $cellCount = 0
    $statusCount = @{ built = 0; skipped = 0; error = 0 }
    try {
        Write-Host 'Starting a hidden Excel...'
        $session = Start-ExcelSession
        $app = $session.App
        $environment = Get-ExcelEnvironment -App $app -DynamicArrays $session.DynamicArrays
        $environment.workbooksBuiltWith = Get-JsonProperty (Get-JsonProperty $manifest 'excel') 'fileVersion'
        Write-Host ("Excel {0} (build {1}), dynamic arrays: {2}" -f $environment.version, $environment.fileVersion, $session.DynamicArrays)

        $n = 0
        foreach ($item in $plan) {
            $n++
            Write-Host ("[{0}/{1}] {2}" -f $n, $plan.Count, $item.Topic)
            $first = $item.Books[0].Record
            $result = [ordered]@{
                excel     = $environment
                schema    = 1
                title     = [string]$first.title
                topic     = $item.Topic
                workbooks = [ordered]@{}
            }
            foreach ($book in $item.Books) {
                $record = $book.Record
                # Open(FileName, UpdateLinks=0, ReadOnly, Format, Password, WriteResPassword,
                #      IgnoreReadOnlyRecommended, Origin, Delimiter, Editable, Notify, Converter, AddToMru)
                $wb = $app.Workbooks.Open($book.Path, 0, $true, [Type]::Missing, [Type]::Missing, [Type]::Missing,
                    $true, [Type]::Missing, [Type]::Missing, $false, $false, [Type]::Missing, $false)
                try {
                    Set-CalculationSettings -App $app -Settings $record.settings
                    $app.CalculateFullRebuild()
                    $label = '{0}.{1}' -f $item.Topic, $record.format
                    $cases = Read-WorkbookCases -Workbook $wb -Record $record -DynamicArrays $session.DynamicArrays `
                        -Differences $differences -CellCount ([ref]$cellCount)
                    foreach ($id in $cases.Keys) { $statusCount[$cases[$id].status]++ }
                    $result.workbooks[[string]$record.format] = [ordered]@{
                        cases    = $cases
                        date1904 = [bool]$wb.Date1904
                        file     = [string]$record.file
                        font     = [string]$record.font
                        settings = $record.settings
                    }
                    Write-Verbose ("{0}: {1} cases" -f $label, $cases.Count)
                }
                finally {
                    try { $wb.Close($false) } catch { }
                    Remove-ComObject $wb
                    $wb = $null
                }
            }
            Write-Utf8LfFile -Path $item.Target -Text (ConvertTo-StableJson $result)
        }
    }
    finally {
        Stop-ExcelSession -Session $session
    }

    Write-Host ("Done in {0:N0} s: {1} topic files, {2} cells. Cases read {3}, skipped {4}, refused at build {5} (counted once per file format)." -f `
            ((Get-Date) - $started).TotalSeconds, $plan.Count, $cellCount, $statusCount.built, $statusCount.skipped, $statusCount.error)
    if ($differences.Count -gt 0) {
        Write-Host ''
        Write-Host ("{0} result(s) differ from what the case notes expected. Excel is the truth here;" -f $differences.Count)
        Write-Host 'these lines only show where a note may be wrong or where Excel is surprising:'
        foreach ($d in $differences) { Write-Host ('  ' + (Protect-PrivateText $d)) }
    }
    else {
        Write-Host 'Every expectation written in the case notes matched Excel.'
    }
    Write-Host 'Results are in the expected folder (or the folder you gave). Review them with git diff before committing.'
}
catch {
    Write-Failure $_
    exit 1
}
