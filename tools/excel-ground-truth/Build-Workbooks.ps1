#Requires -Version 5.1
<#
.SYNOPSIS
Builds one Excel workbook per case file, using Excel itself.

.DESCRIPTION
Reads the case files under -CasesPath, starts a separate hidden Excel, and writes
one workbook per case file into -WorkbookPath (.xlsx, and .xls when the case file
asks for it). Each case gets its own worksheet named case001, case002, ...
A manifest.json next to the workbooks tells Export-GroundTruth.ps1 which sheet
holds which case and which cells to read.

Nothing outside -WorkbookPath is written. Excel options are not changed.

.PARAMETER CasesPath
Folder with the case files. Default: the cases folder next to this script.

.PARAMETER WorkbookPath
Folder for the workbooks and manifest.json. Default: out\workbooks next to this
script (ignored by git). It must be empty or hold only files this tool made.

.PARAMETER Topic
Build only these topics, for example dates/leap-1900. Default: all.

.EXAMPLE
powershell -NoProfile -ExecutionPolicy Bypass -File .\Build-Workbooks.ps1 -WhatIf
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string]$CasesPath,
    [string]$WorkbookPath,
    [string[]]$Topic
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'GroundTruth.psm1') -Force

function Set-InputCell {
    param($Sheet, $Spec)
    $range = $Sheet.Range($Spec.address)
    switch ($Spec.kind) {
        'blank' { [void]$range.ClearContents() }
        'number' { $range.Value2 = [double]$Spec.value }
        'boolean' { $range.Value2 = [bool]$Spec.value }
        # The apostrophe makes Excel keep the text as text ("1" stays text, not the number 1).
        'text' { $range.Value2 = "'" + [string]$Spec.value }
        # Setting Formula to a constant is the same as typing it (en-US rules): Excel parses it.
        'typed' { $range.Formula = [string]$Spec.value }
    }
    if ($null -ne $Spec.format) { $range.NumberFormat = [string]$Spec.format }
    Remove-ComObject $range
}

function Set-FormulaCell {
    param($Sheet, $Spec, [bool]$DynamicArrays)
    $range = $Sheet.Range($Spec.address)
    $set = $Spec.set
    if ($set -eq 'formula2' -and -not $DynamicArrays) { $set = 'formula' }
    try {
        switch ($set) {
            'formula2' { $range.Formula2 = [string]$Spec.formula }
            'formula' { $range.Formula = [string]$Spec.formula }
            'array' { $range.FormulaArray = [string]$Spec.formula }
        }
        if ($null -ne $Spec.format) { $range.NumberFormat = [string]$Spec.format }
    }
    finally {
        Remove-ComObject $range
    }
    return $set
}

function Build-TopicWorkbook {
    # Builds the workbook for one case file and saves it in each requested format.
    # Returns the manifest records (one per format).
    param($Session, $CaseFile, [hashtable]$Targets, [string]$WorkbookRoot)
    $app = $Session.App
    $wb = $app.Workbooks.Add(-4167)
    $records = New-Object System.Collections.ArrayList
    try {
        $wb.Date1904 = [bool]$CaseFile.settings.date1904
        Set-CalculationSettings -App $app -Settings $CaseFile.settings
        try {
            $normal = $wb.Styles.Item('Normal')
            $normal.Font.Name = 'Calibri'
            $normal.Font.Size = 11
            Remove-ComObject $normal
        }
        catch { Write-Verbose 'Could not set the Normal style font; the font in use is recorded instead.' }

        $caseRecords = New-Object System.Collections.ArrayList
        $index = 0
        foreach ($case in $CaseFile.cases) {
            $index++
            $sheetName = 'case{0:D3}' -f $index
            if ($index -eq 1) {
                $ws = $wb.Worksheets.Item(1)
            }
            else {
                $last = $wb.Worksheets.Item($wb.Worksheets.Count)
                $ws = $wb.Worksheets.Add([Type]::Missing, $last)
                Remove-ComObject $last
            }
            $ws.Name = $sheetName
            $ws.StandardWidth = 30

            $record = [ordered]@{
                id       = $case.id
                sheet    = $sheetName
                status   = 'built'
                volatile = $case.volatile
                requires = @($case.requires)
                cells    = New-Object System.Collections.ArrayList
                expect   = $case.expect
            }
            if (@($case.requires) -contains 'dynamic-arrays' -and -not $Session.DynamicArrays) {
                $record.status = 'skipped'
                $record.reason = 'This Excel has no dynamic arrays (needs Microsoft 365 or Excel 2021 or later).'
            }
            else {
                try {
                    foreach ($w in $case.columnWidths) {
                        $col = $ws.Range(('{0}:{0}' -f $w.column))
                        $col.ColumnWidth = [double]$w.width
                        Remove-ComObject $col
                    }
                    foreach ($in in $case.inputs) { Set-InputCell -Sheet $ws -Spec $in }
                    foreach ($f in $case.formulas) {
                        $usedSet = Set-FormulaCell -Sheet $ws -Spec $f -DynamicArrays $Session.DynamicArrays
                        [void]$record.cells.Add([ordered]@{ address = $f.address; set = $usedSet; source = $f.formula })
                    }
                }
                catch {
                    $record.status = 'error'
                    $msg = $_.Exception.Message
                    if ($null -ne $_.Exception.InnerException) { $msg = $_.Exception.InnerException.Message }
                    $record.reason = 'Excel refused the case: ' + (Protect-PrivateText $msg)
                    Write-Warning ("{0} {1}: {2}" -f $CaseFile.topic, $case.id, $record.reason)
                }
            }
            [void]$caseRecords.Add($record)
            Remove-ComObject $ws
            $ws = $null
        }

        $first = $wb.Worksheets.Item(1)
        $first.Activate()
        $fontCell = $first.Range('A1')
        $font = '{0} {1}' -f $fontCell.Font.Name, $fontCell.Font.Size
        Remove-ComObject $fontCell
        Remove-ComObject $first

        $app.CalculateFullRebuild()
        # Saved files open in automatic mode for people; Export recalculates on its own.
        $app.Calculation = -4105
        $wb.RemovePersonalInformation = $true
        $wb.CheckCompatibility = $false

        foreach ($format in $CaseFile.formats) {
            $target = $Targets[$format]
            $dir = [System.IO.Path]::GetDirectoryName($target)
            if (-not [System.IO.Directory]::Exists($dir)) { [void][System.IO.Directory]::CreateDirectory($dir) }
            if ([System.IO.File]::Exists($target)) { [System.IO.File]::Delete($target) }
            $fileFormat = $(if ($format -eq 'xls') { 56 } else { 51 })
            # SaveAs(Filename, FileFormat, Password, WriteResPassword, ReadOnlyRecommended,
            #        CreateBackup, AccessMode=xlNoChange, ConflictResolution=xlLocalSessionChanges, AddToMru)
            $wb.SaveAs($target, $fileFormat, [Type]::Missing, [Type]::Missing, $false, $false, 1, 2, $false)
            [void]$records.Add([ordered]@{
                    cases    = $caseRecords
                    file     = Get-RelativeName -Root $WorkbookRoot -FullName $target
                    font     = $font
                    format   = $format
                    settings = $CaseFile.settings
                    title    = $CaseFile.title
                    topic    = $CaseFile.topic
                })
        }
    }
    finally {
        $app.Calculation = -4135
        try { $wb.Close($false) } catch { }
        Remove-ComObject $wb
        $wb = $null
    }
    return , $records
}

try {
    if (-not $CasesPath) { $CasesPath = Join-Path $PSScriptRoot 'cases' }
    if (-not $WorkbookPath) { $WorkbookPath = Join-Path (Join-Path $PSScriptRoot 'out') 'workbooks' }
    $casesFull = $PSCmdlet.SessionState.Path.GetUnresolvedProviderPathFromPSPath($CasesPath)
    $workbookFull = $PSCmdlet.SessionState.Path.GetUnresolvedProviderPathFromPSPath($WorkbookPath)
    $workbookFull = Assert-WorkFolder -Path $workbookFull -AllowedExtensions @('.xlsx', '.xls', '.json') -Purpose 'workbook'

    $caseFiles = Read-AllCaseFiles -CasesPath $casesFull -Topic $Topic
    $caseCount = 0
    foreach ($cf in $caseFiles) { $caseCount += $cf.cases.Count }
    Write-Host ("Read {0} case files with {1} cases." -f $caseFiles.Count, $caseCount)

    $plan = New-Object System.Collections.ArrayList
    foreach ($cf in $caseFiles) {
        $targets = @{}
        foreach ($format in $cf.formats) {
            $targets[$format] = Resolve-InsideFolder -Root $workbookFull -Relative ($cf.topic + '.' + $format)
        }
        $label = '{0} ({1}; {2} cases)' -f $cf.topic, (($cf.formats | ForEach-Object { '.' + $_ }) -join ', '), $cf.cases.Count
        if ($PSCmdlet.ShouldProcess($label, 'Build workbook with Excel')) {
            [void]$plan.Add([pscustomobject]@{ CaseFile = $cf; Targets = $targets })
        }
    }
    $writeManifest = $PSCmdlet.ShouldProcess('manifest.json', 'Write the list of built workbooks')
    if ($plan.Count -eq 0) {
        Write-Host 'Nothing to build.'
        return
    }

    Assert-ExcelIdle
    $session = $null
    $started = Get-Date
    try {
        Write-Host 'Starting a hidden Excel...'
        $session = Start-ExcelSession
        $environment = Get-ExcelEnvironment -App $session.App -DynamicArrays $session.DynamicArrays
        Write-Host ("Excel {0} (build {1}), dynamic arrays: {2}" -f $environment.version, $environment.fileVersion, $session.DynamicArrays)

        $workbooks = New-Object System.Collections.ArrayList
        $n = 0
        foreach ($item in $plan) {
            $n++
            Write-Host ("[{0}/{1}] {2}" -f $n, $plan.Count, $item.CaseFile.topic)
            $records = Build-TopicWorkbook -Session $session -CaseFile $item.CaseFile -Targets $item.Targets -WorkbookRoot $workbookFull
            foreach ($r in $records) { [void]$workbooks.Add($r) }
        }

        if ($writeManifest) {
            $manifest = [ordered]@{
                excel     = $environment
                schema    = 1
                workbooks = $workbooks
            }
            Write-Utf8LfFile -Path (Join-Path $workbookFull 'manifest.json') -Text (ConvertTo-StableJson $manifest)
        }
    }
    finally {
        Stop-ExcelSession -Session $session
    }

    $statuses = @{ built = 0; skipped = 0; error = 0 }
    foreach ($wbRecord in $workbooks) {
        foreach ($c in $wbRecord.cases) { $statuses[$c.status]++ }
    }
    Write-Host ("Done in {0:N0} s: {1} workbook files; case sheets built {2}, skipped {3}, refused by Excel {4} (counted once per file format)." -f `
            ((Get-Date) - $started).TotalSeconds, $workbooks.Count, $statuses.built, $statuses.skipped, $statuses.error)
    Write-Host 'Workbooks are in out/workbooks (or the folder you gave). Next: Export-GroundTruth.ps1'
}
catch {
    Write-Failure $_
    exit 1
}
