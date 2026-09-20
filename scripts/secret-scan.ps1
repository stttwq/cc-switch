# Phase 0 guardrail (plan section 9 Phase 0, section 10).
# Recursively scans a directory for known fixture secret literals.
# Usage:
#   powershell -NoProfile -File scripts/secret-scan.ps1 -Path <dir> [-Allow fixtures\v18-home]
# Exit 0 = clean; exit 1 = hit found (prints file + literal name).

param(
    [Parameter(Mandatory = $true)][string]$Path,
    [string[]]$Allow = @()
)

$Literals = [ordered]@{
    "sk-fixture-claude-0001"          = "CLAUDE_KEY"
    "sk-fixture-claude-0002"          = "CLAUDE_KEY_ALT_FIELD"
    "sk-fixture-claude-0003"          = "CLAUDE_KEY_EXTRA_ENV"
    "sk-or-fixture-0007"              = "CLAUDE_EXTRA_ENV_OPENROUTER"
    "sk-fixture-codex-official-0001"  = "CODEX_OFFICIAL_KEY"
    "sk-fixture-codex-3rd-0002"       = "CODEX_3RD_KEY"
    "sk-fixture-pi-0003"              = "PI_KEY_1"
    "sk-fixture-pi-0004"              = "PI_KEY_2"
    "sk-fixture-pi-header-0008"       = "PI_HEADER_SECRET"
    "sk-fixture-usage-0005"           = "USAGE_SCRIPT_KEY"
    "sk-fixture-codex-oauth-refresh-0009" = "CODEX_OAUTH_REFRESH_TOKEN"
    "webdav-fixture-pass-0006"        = "WEBDAV_PASSWORD"
}

if (-not (Test-Path $Path)) {
    Write-Error "Path not found: $Path"
    exit 1
}

$fullPath = (Resolve-Path $Path).Path
$allowFull = $Allow | ForEach-Object { Join-Path $fullPath $_ }

$hits = 0
Get-ChildItem -LiteralPath $fullPath -Recurse -File -Force | ForEach-Object {
    $file = $_.FullName
    $skip = $false
    foreach ($a in $allowFull) {
        if ($file.StartsWith($a, [System.StringComparison]::OrdinalIgnoreCase)) { $skip = $true; break }
    }
    if ($skip) { return }

    # Decode once and use .NET's ordinal substring search instead of a hand-written
    # byte-by-byte double loop. The old version cost ~14 s/MB on this machine, which
    # made scanning a repo (let alone one containing build output) take tens of minutes.
    #
    # Byte-for-byte equivalence: every literal above is pure ASCII, and ASCII bytes
    # decode to themselves in UTF-8 regardless of surrounding bytes (a continuation
    # byte can never be < 0x80). Invalid sequences only ever decode to U+FFFD, which
    # is non-ASCII, so this can neither miss a literal nor invent one.
    $text = [System.Text.Encoding]::UTF8.GetString([System.IO.File]::ReadAllBytes($file))
    foreach ($lit in $Literals.Keys) {
        if ($text.Contains($lit)) {
            Write-Host "SECRET HIT: $($Literals[$lit]) ($lit) in $file"
            $script:hits++
        }
    }
}

if ($hits -gt 0) {
    Write-Host "FAIL: $hits secret literal hit(s) under $fullPath"
    exit 1
}
Write-Host "OK: no secret literals under $fullPath"
exit 0
