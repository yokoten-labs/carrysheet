#Requires -Version 5.1
# Shared helpers for Build-Workbooks.ps1 and Export-GroundTruth.ps1.
#
# Keep this file ASCII only: Windows PowerShell 5.1 reads script files without
# a byte order mark in the ANSI code page, so any other character would break.
# Case files (which do contain Chinese and Japanese) are read as UTF-8 explicitly.

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$script:Invariant = [System.Globalization.CultureInfo]::InvariantCulture
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$script:SchemaVersion = 1
$script:Formats = @('xlsx', 'xls')
$script:SetModes = @('formula2', 'formula', 'array')
$script:KnownRequirements = @('dynamic-arrays')

# Excel constants (from the Excel object model).
$script:Xl = @{
    WBATWorksheet          = -4167  # xlWBATWorksheet: a new workbook with one sheet
    CalculationManual      = -4135  # xlCalculationManual
    AutomationForceDisable = 3      # msoAutomationSecurityForceDisable: no macros run
}

# Low 16 bits of the VT_ERROR value Excel returns in Value2 for an error cell.
$script:ErrorNames = @{
    2000 = '#NULL!'
    2007 = '#DIV/0!'
    2015 = '#VALUE!'
    2023 = '#REF!'
    2029 = '#NAME?'
    2036 = '#NUM!'
    2042 = '#N/A'
    2043 = '#GETTING_DATA'
    2045 = '#SPILL!'
    2046 = '#CONNECT!'
    2047 = '#BLOCKED!'
    2048 = '#UNKNOWN!'
    2049 = '#FIELD!'
    2050 = '#CALC!'
}

# ---------------------------------------------------------------------------
# Privacy: nothing that names the person or the machine leaves this script.
# ---------------------------------------------------------------------------

$script:PrivateReplacements = New-Object System.Collections.ArrayList

function Add-PrivateReplacement {
    # -WholeWord is for names: 'ann' must not eat the middle of 'annual'.
    param([string]$Secret, [string]$Placeholder, [switch]$WholeWord)
    if ([string]::IsNullOrEmpty($Secret)) { return }
    $trimmed = $Secret.Trim().TrimEnd('\', '/')
    if ($trimmed.Length -lt 3) { return }
    foreach ($existing in $script:PrivateReplacements) {
        if ($existing.Secret -eq $trimmed) { return }
    }
    $pattern = [regex]::Escape($trimmed)
    if ($WholeWord) { $pattern = '(?<![\p{L}\p{N}_])' + $pattern + '(?![\p{L}\p{N}_])' }
    [void]$script:PrivateReplacements.Add([pscustomobject]@{ Secret = $trimmed; Pattern = $pattern; Placeholder = $Placeholder })
    # Longest first, so a home path is replaced before the bare user name inside it.
    $sorted = @($script:PrivateReplacements | Sort-Object -Property @{ Expression = { $_.Secret.Length } } -Descending)
    $script:PrivateReplacements.Clear()
    foreach ($item in $sorted) { [void]$script:PrivateReplacements.Add($item) }
}

function Initialize-PrivateReplacements {
    Add-PrivateReplacement -Secret $env:USERPROFILE -Placeholder '~'
    Add-PrivateReplacement -Secret $env:OneDrive -Placeholder '~/OneDrive'
    Add-PrivateReplacement -Secret $env:OneDriveCommercial -Placeholder '~/OneDrive'
    Add-PrivateReplacement -Secret $HOME -Placeholder '~'
    if ($env:HOMEDRIVE -and $env:HOMEPATH) {
        Add-PrivateReplacement -Secret ($env:HOMEDRIVE + $env:HOMEPATH) -Placeholder '~'
    }
    Add-PrivateReplacement -Secret $env:USERNAME -Placeholder '<user>' -WholeWord
    Add-PrivateReplacement -Secret $env:USER -Placeholder '<user>' -WholeWord
    Add-PrivateReplacement -Secret $env:USERDOMAIN -Placeholder '<domain>' -WholeWord
    Add-PrivateReplacement -Secret $env:COMPUTERNAME -Placeholder '<computer>' -WholeWord
}

function Protect-PrivateText {
    param([string]$Text)
    if ([string]::IsNullOrEmpty($Text)) { return $Text }
    $result = $Text
    foreach ($item in $script:PrivateReplacements) {
        $result = [regex]::Replace($result, $item.Pattern, $item.Placeholder.Replace('$', '$$'),
            [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)
    }
    return $result
}

Initialize-PrivateReplacements

function Write-Failure {
    # Prints an error without the script position line (which holds a home path).
    param($ErrorRecord)
    $message = $ErrorRecord.Exception.Message
    $inner = $ErrorRecord.Exception.InnerException
    while ($null -ne $inner) {
        if ($inner.Message -and $message -notlike ('*' + $inner.Message + '*')) {
            $message = $message + ' ' + $inner.Message
        }
        $inner = $inner.InnerException
    }
    [Console]::Error.WriteLine('Failed: ' + (Protect-PrivateText $message))
}

# ---------------------------------------------------------------------------
# Files and paths
# ---------------------------------------------------------------------------

function Read-Utf8Text {
    param([string]$Path)
    return [System.IO.File]::ReadAllText($Path, $script:Utf8NoBom)
}

function Write-Utf8LfFile {
    param([string]$Path, [string]$Text)
    $dir = [System.IO.Path]::GetDirectoryName($Path)
    if (-not [System.IO.Directory]::Exists($dir)) { [void][System.IO.Directory]::CreateDirectory($dir) }
    $normalized = $Text.Replace("`r`n", "`n")
    if (-not $normalized.EndsWith("`n")) { $normalized += "`n" }
    [System.IO.File]::WriteAllText($Path, $normalized, $script:Utf8NoBom)
}

function Get-RelativeName {
    # 'dates/leap-1900' style name of a file below a root, with forward slashes.
    param([string]$Root, [string]$FullName)
    $rootFull = [System.IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
    $full = [System.IO.Path]::GetFullPath($FullName)
    if (-not $full.StartsWith($rootFull, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Internal error: a file is not inside the folder it was listed from."
    }
    return $full.Substring($rootFull.Length).TrimStart('\', '/').Replace('\', '/')
}

function Resolve-InsideFolder {
    # Joins a relative name to a root and refuses anything that would leave the root.
    param([string]$Root, [string]$Relative)
    if ([string]::IsNullOrEmpty($Relative)) { throw 'An empty file name was given.' }
    if ([System.IO.Path]::IsPathRooted($Relative) -or $Relative.Contains(':')) {
        throw "'$Relative' is not a relative name. Only files inside the given folder are used."
    }
    $parts = $Relative.Split([char[]]@('/', '\'))
    foreach ($part in $parts) {
        if ($part -eq '..' -or $part -eq '.' -or $part -eq '') {
            throw "'$Relative' is not a plain relative name. Only files inside the given folder are used."
        }
    }
    $rootFull = [System.IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
    $sep = [System.IO.Path]::DirectorySeparatorChar
    $full = [System.IO.Path]::GetFullPath($rootFull + $sep + ($parts -join $sep))
    if (-not $full.StartsWith($rootFull + $sep, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "'$Relative' points outside the given folder."
    }
    return $full
}

function Assert-WorkFolder {
    # Refuses folders where writing or deleting workbooks could hurt: drive roots,
    # the profile, Desktop and Documents, and folders holding other kinds of files.
    param([string]$Path, [string[]]$AllowedExtensions, [string]$Purpose)
    $full = [System.IO.Path]::GetFullPath($Path).TrimEnd('\', '/')
    $root = [System.IO.Path]::GetPathRoot($full)
    if ([string]::IsNullOrEmpty($full) -or $full.Length -le $root.TrimEnd('\', '/').Length) {
        throw "The $Purpose folder cannot be a drive root. Pick an empty folder or keep the default."
    }
    $special = @(
        [Environment]::GetFolderPath('UserProfile'),
        [Environment]::GetFolderPath('Desktop'),
        [Environment]::GetFolderPath('MyDocuments')
    )
    foreach ($s in $special) {
        if ($s -and $full -ieq $s.TrimEnd('\', '/')) {
            throw "The $Purpose folder cannot be your profile, Desktop or Documents folder. Pick an empty folder or keep the default."
        }
    }
    if ([System.IO.Directory]::Exists($full)) {
        $others = @(Get-ChildItem -LiteralPath $full -Recurse -File -Force |
            Where-Object { $AllowedExtensions -notcontains $_.Extension.ToLowerInvariant() })
        if ($others.Count -gt 0) {
            throw ("The $Purpose folder already holds $($others.Count) file(s) that this tool did not make " +
                "(for example '$(Get-RelativeName -Root $full -FullName $others[0].FullName)'). " +
                "Pick an empty folder or keep the default.")
        }
    }
    return $full
}

# ---------------------------------------------------------------------------
# Reading JSON (works with both ConvertFrom-Json flavours)
# ---------------------------------------------------------------------------

function Get-JsonProperty {
    param($Object, [string]$Name, $Default = $null)
    if ($null -eq $Object) { return $Default }
    $p = $Object.PSObject.Properties[$Name]
    if ($null -eq $p) { return $Default }
    return , $p.Value
}

function Test-JsonNumber {
    param($Value)
    if ($null -eq $Value) { return $false }
    if ($Value -is [int] -or $Value -is [long] -or $Value -is [double] -or $Value -is [decimal] -or $Value -is [single]) {
        return $true
    }
    return ($Value.GetType().FullName -eq 'System.Numerics.BigInteger')
}

function Test-JsonObject {
    param($Value)
    if ($null -eq $Value) { return $false }
    return ($Value.GetType().FullName -eq 'System.Management.Automation.PSCustomObject')
}

function Get-JsonPropertyNames {
    param($Object)
    if (-not (Test-JsonObject $Object)) { return @() }
    return @($Object.PSObject.Properties | ForEach-Object { $_.Name })
}

# ---------------------------------------------------------------------------
# Writing JSON: sorted keys, two-space indent, LF, numbers that round-trip
# ---------------------------------------------------------------------------

function Format-JsonDouble {
    param([double]$Number)
    if ([double]::IsNaN($Number) -or [double]::IsInfinity($Number)) {
        throw 'Internal error: a number that JSON cannot hold (NaN or infinity).'
    }
    $text = $Number.ToString('R', $script:Invariant)
    if ([double]::Parse($text, $script:Invariant) -ne $Number) {
        $text = $Number.ToString('G17', $script:Invariant)
    }
    return $text
}

function Add-JsonString {
    param([System.Text.StringBuilder]$Builder, [string]$Text)
    $Text = Protect-PrivateText $Text
    [void]$Builder.Append('"')
    for ($i = 0; $i -lt $Text.Length; $i++) {
        $c = $Text[$i]
        $code = [int]$c
        if ($code -eq 34) { [void]$Builder.Append('\"') }
        elseif ($code -eq 92) { [void]$Builder.Append('\\') }
        elseif ($code -eq 10) { [void]$Builder.Append('\n') }
        elseif ($code -eq 13) { [void]$Builder.Append('\r') }
        elseif ($code -eq 9) { [void]$Builder.Append('\t') }
        elseif ($code -lt 32 -or $code -eq 127) { [void]$Builder.Append(('\u{0:x4}' -f $code)) }
        elseif ([char]::IsHighSurrogate($c)) {
            if ($i + 1 -lt $Text.Length -and [char]::IsLowSurrogate($Text[$i + 1])) {
                [void]$Builder.Append($c).Append($Text[$i + 1])
                $i++
            }
            else {
                # A lone surrogate (for example LEFT of an emoji) cannot be written as UTF-8.
                [void]$Builder.Append(('\u{0:x4}' -f $code))
            }
        }
        elseif ([char]::IsLowSurrogate($c)) { [void]$Builder.Append(('\u{0:x4}' -f $code)) }
        else { [void]$Builder.Append($c) }
    }
    [void]$Builder.Append('"')
}

function Add-JsonValue {
    param([System.Text.StringBuilder]$Builder, $Value, [int]$Indent)
    $pad = ' ' * (2 * ($Indent + 1))
    $endPad = ' ' * (2 * $Indent)

    if ($null -eq $Value) { [void]$Builder.Append('null'); return }
    if ($Value -is [bool]) { [void]$Builder.Append($(if ($Value) { 'true' } else { 'false' })); return }
    if ($Value -is [string] -or $Value -is [char]) { Add-JsonString -Builder $Builder -Text ([string]$Value); return }
    if ($Value -is [double] -or $Value -is [single]) { [void]$Builder.Append((Format-JsonDouble ([double]$Value))); return }
    if ($Value -is [int] -or $Value -is [long] -or $Value -is [int16] -or $Value -is [byte] -or
        $Value -is [uint16] -or $Value -is [uint32] -or $Value -is [uint64] -or $Value -is [sbyte] -or $Value -is [decimal]) {
        [void]$Builder.Append($Value.ToString($script:Invariant))
        return
    }

    $dict = $null
    if ($Value -is [System.Collections.IDictionary]) { $dict = $Value }
    elseif (Test-JsonObject $Value) {
        $dict = @{}
        foreach ($p in $Value.PSObject.Properties) { $dict[$p.Name] = $p.Value }
    }
    if ($null -ne $dict) {
        [string[]]$keys = @($dict.Keys | ForEach-Object { [string]$_ })
        if ($keys.Count -eq 0) { [void]$Builder.Append('{}'); return }
        [Array]::Sort($keys, [System.StringComparer]::Ordinal)
        [void]$Builder.Append("{`n")
        for ($k = 0; $k -lt $keys.Count; $k++) {
            [void]$Builder.Append($pad)
            Add-JsonString -Builder $Builder -Text $keys[$k]
            [void]$Builder.Append(': ')
            Add-JsonValue -Builder $Builder -Value $dict[$keys[$k]] -Indent ($Indent + 1)
            if ($k -lt $keys.Count - 1) { [void]$Builder.Append(',') }
            [void]$Builder.Append("`n")
        }
        [void]$Builder.Append($endPad).Append('}')
        return
    }

    if ($Value -is [System.Collections.IEnumerable]) {
        $items = @($Value)
        if ($items.Count -eq 0) { [void]$Builder.Append('[]'); return }
        [void]$Builder.Append("[`n")
        for ($k = 0; $k -lt $items.Count; $k++) {
            [void]$Builder.Append($pad)
            Add-JsonValue -Builder $Builder -Value $items[$k] -Indent ($Indent + 1)
            if ($k -lt $items.Count - 1) { [void]$Builder.Append(',') }
            [void]$Builder.Append("`n")
        }
        [void]$Builder.Append($endPad).Append(']')
        return
    }

    Add-JsonString -Builder $Builder -Text ([string]$Value)
}

function ConvertTo-StableJson {
    param($InputObject)
    $builder = New-Object System.Text.StringBuilder
    Add-JsonValue -Builder $builder -Value $InputObject -Indent 0
    return $builder.ToString()
}

# ---------------------------------------------------------------------------
# Case files
# ---------------------------------------------------------------------------

$script:CellPattern = '^[A-Z]{1,3}[1-9][0-9]{0,6}$'
$script:RangePattern = '^[A-Z]{1,3}[1-9][0-9]{0,6}(:[A-Z]{1,3}[1-9][0-9]{0,6})?$'
$script:IdPattern = '^[a-z0-9]+([.-][a-z0-9]+)*$'

function ConvertTo-InputSpec {
    param([string]$Address, $Raw, [string]$Where, [System.Collections.ArrayList]$Problems)
    $spec = [ordered]@{ address = $Address; kind = $null; value = $null; format = $null }
    if ($Address -cnotmatch $script:CellPattern) {
        [void]$Problems.Add("${Where}: input address '$Address' is not a single cell like B2.")
        return $null
    }
    $payload = $Raw
    if (Test-JsonObject $Raw) {
        $names = Get-JsonPropertyNames $Raw
        foreach ($n in $names) {
            if (@('value', 'typed', 'format') -notcontains $n) {
                [void]$Problems.Add("${Where}: input $Address has an unknown key '$n' (use value, typed or format).")
            }
        }
        $hasValue = $names -contains 'value'
        $hasTyped = $names -contains 'typed'
        if ($hasValue -eq $hasTyped) {
            [void]$Problems.Add("${Where}: input $Address needs exactly one of 'value' or 'typed'.")
            return $null
        }
        $format = Get-JsonProperty $Raw 'format'
        if ($null -ne $format) {
            if ($format -isnot [string]) { [void]$Problems.Add("${Where}: input $Address format must be text.") }
            $spec.format = [string]$format
        }
        if ($hasTyped) {
            $typed = Get-JsonProperty $Raw 'typed'
            if ($typed -isnot [string] -or $typed.Length -eq 0 -or $typed.StartsWith('=')) {
                [void]$Problems.Add("${Where}: input $Address 'typed' must be non-empty text that does not start with '=' (put formulas under formulas).")
                return $null
            }
            $spec.kind = 'typed'
            $spec.value = $typed
            return $spec
        }
        $payload = Get-JsonProperty $Raw 'value'
    }
    if ($null -eq $payload) { $spec.kind = 'blank' }
    elseif ($payload -is [bool]) { $spec.kind = 'boolean'; $spec.value = [bool]$payload }
    elseif (Test-JsonNumber $payload) { $spec.kind = 'number'; $spec.value = [double]$payload }
    elseif ($payload -is [string]) {
        if ($payload.Length -eq 0) {
            [void]$Problems.Add("${Where}: input $Address is empty text, which Excel cannot store as a constant; use a formula =`"`" instead.")
            return $null
        }
        $spec.kind = 'text'
        $spec.value = $payload
    }
    elseif ($payload -is [datetime]) {
        [void]$Problems.Add("${Where}: input $Address was read as a date by PowerShell; write it as {`"typed`": `"...`"} instead.")
        return $null
    }
    else {
        [void]$Problems.Add("${Where}: input $Address must be a number, true/false, text, null, or an object with value/typed.")
        return $null
    }
    return $spec
}

function ConvertTo-FormulaSpec {
    param([string]$Address, $Raw, [string]$Where, [System.Collections.ArrayList]$Problems)
    $spec = [ordered]@{ address = $Address; formula = $null; set = 'formula2'; format = $null }
    if ($Raw -is [string]) { $spec.formula = $Raw }
    elseif (Test-JsonObject $Raw) {
        foreach ($n in (Get-JsonPropertyNames $Raw)) {
            if (@('f', 'set', 'format') -notcontains $n) {
                [void]$Problems.Add("${Where}: formula $Address has an unknown key '$n' (use f, set or format).")
            }
        }
        $spec.formula = Get-JsonProperty $Raw 'f'
        $set = Get-JsonProperty $Raw 'set' 'formula2'
        if ($script:SetModes -notcontains $set) {
            [void]$Problems.Add("${Where}: formula $Address 'set' must be formula2, formula or array.")
            return $null
        }
        $spec.set = $set
        $format = Get-JsonProperty $Raw 'format'
        if ($null -ne $format) { $spec.format = [string]$format }
    }
    else {
        [void]$Problems.Add("${Where}: formula $Address must be text or an object with f.")
        return $null
    }
    if ($spec.formula -isnot [string] -or -not $spec.formula.StartsWith('=')) {
        [void]$Problems.Add("${Where}: formula $Address must start with '='.")
        return $null
    }
    $pattern = $(if ($spec.set -eq 'array') { $script:RangePattern } else { $script:CellPattern })
    if ($Address -cnotmatch $pattern) {
        [void]$Problems.Add("${Where}: formula address '$Address' is not valid (a single cell, or a range for set=array).")
        return $null
    }
    return $spec
}

function Read-InputMap {
    param($Map, [string]$Where, [System.Collections.ArrayList]$Problems)
    $list = New-Object System.Collections.ArrayList
    if ($null -eq $Map) { return , $list }
    if (-not (Test-JsonObject $Map)) {
        [void]$Problems.Add("${Where}: inputs must be an object of cell -> value.")
        return , $list
    }
    foreach ($p in $Map.PSObject.Properties) {
        $spec = ConvertTo-InputSpec -Address $p.Name -Raw $p.Value -Where $Where -Problems $Problems
        if ($null -ne $spec) { [void]$list.Add($spec) }
    }
    return , $list
}

function Read-CaseFile {
    # Reads and checks one case file. Returns a hashtable; problems go to $Problems.
    param([string]$Path, [string]$Topic, [System.Collections.ArrayList]$Problems)
    $where = "cases/$Topic.json"
    try {
        $json = ConvertFrom-Json -InputObject (Read-Utf8Text $Path)
    }
    catch {
        [void]$Problems.Add("${where}: not valid JSON ($($_.Exception.Message)).")
        return $null
    }
    $schema = Get-JsonProperty $json 'schema'
    if ($schema -ne $script:SchemaVersion) { [void]$Problems.Add("${where}: 'schema' must be $($script:SchemaVersion).") }
    $title = Get-JsonProperty $json 'title'
    if ($title -isnot [string] -or $title.Length -eq 0) { [void]$Problems.Add("${where}: 'title' is missing.") }

    $formats = @(Get-JsonProperty $json 'formats' @('xlsx'))
    foreach ($f in $formats) {
        if ($script:Formats -notcontains $f) { [void]$Problems.Add("${where}: format '$f' is not xlsx or xls.") }
    }
    $formats = @($script:Formats | Where-Object { $formats -contains $_ })

    $rawSettings = Get-JsonProperty $json 'settings'
    $settings = [ordered]@{
        date1904      = [bool](Get-JsonProperty $rawSettings 'date1904' $false)
        iteration     = [bool](Get-JsonProperty $rawSettings 'iteration' $false)
        maxChange     = [double](Get-JsonProperty $rawSettings 'maxChange' 0.001)
        maxIterations = [int](Get-JsonProperty $rawSettings 'maxIterations' 100)
    }
    foreach ($n in (Get-JsonPropertyNames $rawSettings)) {
        if (@('date1904', 'iteration', 'maxIterations', 'maxChange') -notcontains $n) {
            [void]$Problems.Add("${where}: unknown setting '$n'.")
        }
    }

    $fileRequires = @(Get-JsonProperty $json 'requires' @())
    $sharedInputs = Read-InputMap -Map (Get-JsonProperty $json 'inputs') -Where $where -Problems $Problems

    $cases = New-Object System.Collections.ArrayList
    $rawCases = @(Get-JsonProperty $json 'cases' @())
    if ($rawCases.Count -eq 0) { [void]$Problems.Add("${where}: 'cases' is empty.") }
    foreach ($rc in $rawCases) {
        $id = Get-JsonProperty $rc 'id'
        $cw = "$where case '$id'"
        if ($id -isnot [string] -or $id -cnotmatch $script:IdPattern) {
            [void]$Problems.Add("${where}: case id '$id' must be lower-case letters, digits, dots and dashes.")
            continue
        }
        foreach ($n in (Get-JsonPropertyNames $rc)) {
            if (@('id', 'note', 'inputs', 'formulas', 'expect', 'volatile', 'requires', 'columnWidths') -notcontains $n) {
                [void]$Problems.Add("${cw}: unknown key '$n'.")
            }
        }
        $note = Get-JsonProperty $rc 'note'
        if ($note -isnot [string] -or $note.Length -lt 10) { [void]$Problems.Add("${cw}: 'note' must say why this is a trap.") }

        $own = Read-InputMap -Map (Get-JsonProperty $rc 'inputs') -Where $cw -Problems $Problems
        $ownAddresses = @($own | ForEach-Object { $_.address })
        $inputs = New-Object System.Collections.ArrayList
        foreach ($s in $sharedInputs) { if ($ownAddresses -notcontains $s.address) { [void]$inputs.Add($s) } }
        foreach ($s in $own) { [void]$inputs.Add($s) }

        $formulas = New-Object System.Collections.ArrayList
        $rawFormulas = Get-JsonProperty $rc 'formulas'
        if (-not (Test-JsonObject $rawFormulas) -or @(Get-JsonPropertyNames $rawFormulas).Count -eq 0) {
            [void]$Problems.Add("${cw}: 'formulas' must list at least one cell.")
        }
        else {
            foreach ($p in $rawFormulas.PSObject.Properties) {
                $spec = ConvertTo-FormulaSpec -Address $p.Name -Raw $p.Value -Where $cw -Problems $Problems
                if ($null -ne $spec) { [void]$formulas.Add($spec) }
            }
        }

        $requires = @($fileRequires) + @(Get-JsonProperty $rc 'requires' @())
        $requires = @($requires | Where-Object { $null -ne $_ } | Sort-Object -Unique)
        foreach ($r in $requires) {
            if ($script:KnownRequirements -notcontains $r) { [void]$Problems.Add("${cw}: unknown requirement '$r'.") }
        }

        $widths = New-Object System.Collections.ArrayList
        $rawWidths = Get-JsonProperty $rc 'columnWidths'
        foreach ($n in (Get-JsonPropertyNames $rawWidths)) {
            $w = Get-JsonProperty $rawWidths $n
            if ($n -cnotmatch '^[A-Z]{1,3}$' -or -not (Test-JsonNumber $w) -or [double]$w -lt 0 -or [double]$w -gt 255) {
                [void]$Problems.Add("${cw}: column width '$n' must be a column letter with a width from 0 to 255.")
                continue
            }
            [void]$widths.Add([ordered]@{ column = $n; width = [double]$w })
        }

        $expect = [ordered]@{}
        $rawExpect = Get-JsonProperty $rc 'expect'
        foreach ($n in (Get-JsonPropertyNames $rawExpect)) {
            $e = Get-JsonProperty $rawExpect $n
            if ($n -cnotmatch $script:CellPattern) { [void]$Problems.Add("${cw}: expect address '$n' is not a cell.") }
            elseif ($e -is [bool] -or $e -is [string]) { $expect[$n] = $e }
            elseif (Test-JsonNumber $e) { $expect[$n] = [double]$e }
            else { [void]$Problems.Add("${cw}: expect $n must be a number, true/false or text.") }
        }

        [void]$cases.Add([ordered]@{
                id           = $id
                note         = $note
                inputs       = $inputs
                formulas     = $formulas
                requires     = $requires
                volatile     = [bool](Get-JsonProperty $rc 'volatile' $false)
                columnWidths = $widths
                expect       = $expect
            })
    }

    return [ordered]@{
        topic    = $Topic
        title    = $title
        formats  = $formats
        settings = $settings
        cases    = $cases
    }
}

function Read-AllCaseFiles {
    # Reads every *.json under the cases folder (optionally only some topics) and
    # checks that case ids are unique across all files. Throws with every problem found.
    param([string]$CasesPath, [string[]]$Topic)
    if (-not [System.IO.Directory]::Exists($CasesPath)) { throw "The cases folder does not exist: $(Split-Path -Leaf $CasesPath)" }
    $files = @(Get-ChildItem -LiteralPath $CasesPath -Recurse -File -Filter '*.json' |
        ForEach-Object {
            [pscustomobject]@{
                Topic = (Get-RelativeName -Root $CasesPath -FullName $_.FullName) -replace '\.json$', ''
                Path  = $_.FullName
            }
        } | Sort-Object -Property Topic)
    if ($Topic) { $files = @($files | Where-Object { $Topic -contains $_.Topic }) }
    if ($files.Count -eq 0) { throw 'No case files matched. Check -CasesPath and -Topic (topics look like dates/leap-1900).' }

    $problems = New-Object System.Collections.ArrayList
    $result = New-Object System.Collections.ArrayList
    $seen = @{}
    foreach ($f in $files) {
        $cf = Read-CaseFile -Path $f.Path -Topic $f.Topic -Problems $problems
        if ($null -eq $cf) { continue }
        foreach ($c in $cf.cases) {
            if ($seen.ContainsKey($c.id)) { [void]$problems.Add("case id '$($c.id)' is used twice ($($seen[$c.id]) and $($f.Topic)).") }
            else { $seen[$c.id] = $f.Topic }
        }
        [void]$result.Add($cf)
    }
    if ($problems.Count -gt 0) {
        throw ("The case files have $($problems.Count) problem(s); nothing was started:`n  " + ($problems -join "`n  "))
    }
    return , $result
}

# ---------------------------------------------------------------------------
# Excel session
# ---------------------------------------------------------------------------

$script:NativeSource = @'
using System;
using System.Runtime.InteropServices;

namespace CarrysheetGroundTruth
{
    public static class Native
    {
        [DllImport("ole32.dll", CharSet = CharSet.Unicode)]
        private static extern int CLSIDFromProgID(string progId, out Guid clsid);

        [DllImport("oleaut32.dll")]
        private static extern int GetActiveObject(ref Guid clsid, IntPtr reserved,
            [MarshalAs(UnmanagedType.IUnknown)] out object instance);

        [DllImport("user32.dll")]
        private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);

        public static object GetRunning(string progId)
        {
            Guid clsid;
            if (CLSIDFromProgID(progId, out clsid) != 0) { return null; }
            object instance;
            if (GetActiveObject(ref clsid, IntPtr.Zero, out instance) != 0) { return null; }
            return instance;
        }

        public static int ProcessIdOfWindow(long window)
        {
            uint processId;
            GetWindowThreadProcessId(new IntPtr(window), out processId);
            return (int)processId;
        }
    }
}
'@

function Initialize-Native {
    if (-not ('CarrysheetGroundTruth.Native' -as [type])) {
        Add-Type -TypeDefinition $script:NativeSource -Language CSharp
    }
}

function Remove-ComObject {
    param($Object)
    if ($null -eq $Object) { return }
    try {
        if ([System.Runtime.InteropServices.Marshal]::IsComObject($Object)) {
            [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($Object)
        }
    }
    catch { }
}

function Assert-ExcelIdle {
    # Refuses to start if an Excel the person is using has unsaved work.
    $running = @(Get-Process -Name 'EXCEL' -ErrorAction SilentlyContinue)
    if ($running.Count -eq 0) { return }
    if ($running.Count -gt 1) {
        throw "Excel is running $($running.Count) times. Save your work, close Excel (check Task Manager for hidden copies), then run this again. Nothing was changed."
    }
    $app = $null
    try { Initialize-Native; $app = [CarrysheetGroundTruth.Native]::GetRunning('Excel.Application') } catch { $app = $null }
    if ($null -eq $app) {
        throw 'Excel is running, but this script cannot check it for unsaved work. Save your work, close Excel, then run this again. Nothing was changed.'
    }
    $unsaved = 0
    try {
        $books = $app.Workbooks
        for ($i = 1; $i -le $books.Count; $i++) {
            $b = $books.Item($i)
            if (-not $b.Saved) { $unsaved++ }
            Remove-ComObject $b
        }
        Remove-ComObject $books
    }
    catch {
        throw 'Excel is busy (a cell may be in edit mode or a dialog is open). Finish that or close Excel, then run this again. Nothing was changed.'
    }
    finally {
        Remove-ComObject $app
        $app = $null
    }
    if ($unsaved -gt 0) {
        throw "Excel has $unsaved workbook(s) with unsaved changes. Save or close them, then run this again. Nothing was changed."
    }
    Write-Host 'Excel is open with no unsaved work. This tool uses its own hidden copy of Excel and does not touch your windows.'
}

function Start-ExcelSession {
    # Starts a separate hidden Excel. Returns an object for Stop-ExcelSession.
    $session = [pscustomobject]@{
        App           = $null
        Owned         = $false
        ProcessId     = 0
        Anchor        = $null
        DynamicArrays = $false
    }
    $session.App = New-Object -ComObject 'Excel.Application'
    try {
        $app = $session.App
        if ($app.Visible -or $app.Workbooks.Count -gt 0) {
            throw 'Windows handed this script an Excel that is already in use, so it stopped without changing anything. Close Excel and run this again.'
        }
        $session.Owned = $true
        try { Initialize-Native; $session.ProcessId = [CarrysheetGroundTruth.Native]::ProcessIdOfWindow([long]$app.Hwnd) } catch { $session.ProcessId = 0 }
        $app.DisplayAlerts = $false
        $app.Visible = $false
        $app.ScreenUpdating = $false
        $app.EnableEvents = $false
        $app.AskToUpdateLinks = $false
        $app.AutomationSecurity = $script:Xl.AutomationForceDisable
        try { $app.UserControl = $false } catch { }
        Add-PrivateReplacement -Secret ([string]$app.UserName) -Placeholder '<user>' -WholeWord
        Add-PrivateReplacement -Secret ([string]$app.OrganizationName) -Placeholder '<organization>' -WholeWord

        # An empty workbook that stays open: the session takes its calculation mode
        # from the first workbook, so later workbooks open without recalculating.
        $session.Anchor = $app.Workbooks.Add($script:Xl.WBATWorksheet)
        $app.Calculation = $script:Xl.CalculationManual
        # Dynamic arrays exist when Range.Formula2 can be set (Microsoft 365, Excel 2021 and later).
        $probe = $session.Anchor.Worksheets.Item(1).Range('A1')
        # Setting a property that does not exist always throws, so this is a real test.
        try { $probe.Formula2 = '=1'; $session.DynamicArrays = $true } catch { $session.DynamicArrays = $false }
        try { [void]$probe.ClearContents() } catch { }
        Remove-ComObject $probe
    }
    catch {
        Stop-ExcelSession -Session $session
        throw
    }
    return $session
}

function Stop-ExcelSession {
    # Closes every workbook without saving, quits the Excel this tool started,
    # releases COM references, and ends that one process if it does not exit.
    param($Session)
    if ($null -eq $Session -or $null -eq $Session.App) { return }
    $app = $Session.App
    if ($Session.Owned) {
        try {
            $guard = 0
            while ($app.Workbooks.Count -gt 0 -and $guard -lt 1000) {
                $w = $app.Workbooks.Item(1)
                $w.Close($false)
                Remove-ComObject $w
                $guard++
            }
        }
        catch { }
        try { $app.Quit() } catch { }
    }
    Remove-ComObject $Session.Anchor
    Remove-ComObject $app
    $Session.Anchor = $null
    $Session.App = $null
    $app = $null
    [GC]::Collect()
    [GC]::WaitForPendingFinalizers()
    [GC]::Collect()
    [GC]::WaitForPendingFinalizers()
    if ($Session.Owned -and $Session.ProcessId -gt 0) {
        $p = Get-Process -Id $Session.ProcessId -ErrorAction SilentlyContinue
        if ($null -ne $p -and $p.ProcessName -eq 'EXCEL') {
            if (-not $p.WaitForExit(15000)) {
                Write-Warning 'The hidden Excel did not close in 15 seconds; ending that one process.'
                Stop-Process -Id $Session.ProcessId -Force -ErrorAction SilentlyContinue
            }
        }
    }
}

function Get-SafeValue {
    param([scriptblock]$Block)
    try { return (& $Block) } catch { return $null }
}

function Get-ExcelEnvironment {
    # Version, locale and separators. No user name, organization or path.
    param($App, [bool]$DynamicArrays)
    $fileVersion = Get-SafeValue { (Get-Item -LiteralPath (Join-Path ([string]$App.Path) 'EXCEL.EXE')).VersionInfo.ProductVersion }
    return [ordered]@{
        build               = [string](Get-SafeValue { $App.Build })
        calculationVersion  = Get-SafeValue { [long]$App.CalculationVersion }
        decimalSeparator    = Get-SafeValue { [string]$App.DecimalSeparator }
        dynamicArrays       = $DynamicArrays
        fileVersion         = $fileVersion
        international       = [ordered]@{
            countryCode        = Get-SafeValue { $App.International(1) }
            countrySetting     = Get-SafeValue { $App.International(2) }
            dateOrder          = Get-SafeValue { $App.International(32) }
            dateSeparator      = Get-SafeValue { $App.International(17) }
            decimalSeparator   = Get-SafeValue { $App.International(3) }
            fourDigitYears     = Get-SafeValue { $App.International(43) }
            listSeparator      = Get-SafeValue { $App.International(5) }
            thousandsSeparator = Get-SafeValue { $App.International(4) }
            timeSeparator      = Get-SafeValue { $App.International(18) }
            twentyFourHourClock = Get-SafeValue { $App.International(33) }
        }
        languages           = [ordered]@{
            exeMode = Get-SafeValue { $App.LanguageSettings.LanguageID(4) }
            install = Get-SafeValue { $App.LanguageSettings.LanguageID(1) }
            ui      = Get-SafeValue { $App.LanguageSettings.LanguageID(2) }
        }
        operatingSystem     = Get-SafeValue { [string]$App.OperatingSystem }
        powershell          = $PSVersionTable.PSVersion.ToString()
        thousandsSeparator  = Get-SafeValue { [string]$App.ThousandsSeparator }
        useSystemSeparators = Get-SafeValue { [bool]$App.UseSystemSeparators }
        version             = Get-SafeValue { [string]$App.Version }
        windowsCulture      = (Get-Culture).Name
        windowsUiCulture    = (Get-UICulture).Name
    }
}

function Set-CalculationSettings {
    param($App, $Settings)
    $iteration = [bool](Get-SettingValue $Settings 'iteration' $false)
    $App.Iteration = $iteration
    if ($iteration) {
        $App.MaxIterations = [int](Get-SettingValue $Settings 'maxIterations' 100)
        $App.MaxChange = [double](Get-SettingValue $Settings 'maxChange' 0.001)
    }
}

function Get-SettingValue {
    # Settings come as a hashtable (from case files) or a PSCustomObject (from the manifest).
    param($Settings, [string]$Name, $Default)
    if ($null -eq $Settings) { return $Default }
    if ($Settings -is [System.Collections.IDictionary]) {
        if ($Settings.Contains($Name)) { return $Settings[$Name] }
        return $Default
    }
    return (Get-JsonProperty $Settings $Name $Default)
}

# ---------------------------------------------------------------------------
# Reading cells
# ---------------------------------------------------------------------------

function Get-CellAddress {
    param($Range)
    return [string]$Range.Address($false, $false)
}

function Read-ExcelCell {
    # One cell: Value2 with its type, the displayed text, number format and formula.
    param($Cell, [bool]$DynamicArrays)
    $entry = [ordered]@{}
    $v = $Cell.Value2
    if ($null -eq $v) { $entry.type = 'empty'; $entry.value = $null }
    elseif ($v -is [string]) { $entry.type = 'string'; $entry.value = $v }
    elseif ($v -is [bool]) { $entry.type = 'boolean'; $entry.value = $v }
    elseif ($v -is [double]) { $entry.type = 'number'; $entry.value = $v }
    elseif ($v -is [int]) {
        # Errors arrive as VT_ERROR, which .NET turns into an Int32; numbers always arrive as Double.
        $code = $v -band 0xFFFF
        $entry.type = 'error'
        $entry.errorCode = $code
        if ($script:ErrorNames.ContainsKey($code)) { $entry.value = $script:ErrorNames[$code] }
        else { $entry.value = [string]$Cell.Text }
    }
    else { $entry.type = 'unknown'; $entry.value = [string]$v }
    $entry.text = [string]$Cell.Text
    $entry.numberFormat = [string]$Cell.NumberFormat
    if ([bool]$Cell.HasFormula) {
        if ($DynamicArrays) { $entry.formula = [string]$Cell.Formula2 }
        else { $entry.formula = [string]$Cell.Formula }
    }
    try {
        if ([bool]$Cell.HasArray) { $entry.arrayRange = Get-CellAddress $Cell.CurrentArray }
    }
    catch { }
    return $entry
}

function Test-Expectation {
    # Compares one author expectation with what Excel returned. Used only for the console summary.
    param($Expected, $Entry)
    if ($null -eq $Entry) { return $false }
    if ($Expected -is [bool]) { return ($Entry.type -eq 'boolean' -and $Entry.value -eq $Expected) }
    if ($Expected -is [double]) {
        if ($Entry.type -ne 'number') { return $false }
        $tolerance = 1e-15 * [Math]::Max(1.0, [Math]::Abs($Expected))
        return ([Math]::Abs([double]$Entry.value - $Expected) -le $tolerance)
    }
    if ($Expected -is [string]) {
        return (($Entry.type -eq 'string' -or $Entry.type -eq 'error') -and [string]$Entry.value -ceq $Expected)
    }
    return $false
}

Export-ModuleMember -Function @(
    'Add-PrivateReplacement', 'Protect-PrivateText', 'Write-Failure',
    'Read-Utf8Text', 'Write-Utf8LfFile', 'Get-RelativeName', 'Resolve-InsideFolder', 'Assert-WorkFolder',
    'Get-JsonProperty', 'ConvertTo-StableJson',
    'Read-AllCaseFiles',
    'Remove-ComObject', 'Assert-ExcelIdle', 'Start-ExcelSession', 'Stop-ExcelSession',
    'Get-ExcelEnvironment', 'Set-CalculationSettings', 'Get-SettingValue',
    'Get-CellAddress', 'Read-ExcelCell', 'Test-Expectation'
) -Variable @()
