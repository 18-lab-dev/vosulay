# Isolated policy tests; actual Authenticode trust still requires signed Windows artifacts.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'verify-release.ps1'), [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
$function = $ast.Find({ param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $node.Name -eq 'Assert-Signature'
}, $true)
if ($null -eq $function) { throw 'Assert-Signature function missing' }
Invoke-Expression $function.Extent.Text

function Get-AuthenticodeSignature { param([string]$LiteralPath) $script:signature }
function New-TestCertificate([string]$Name) {
    $certificate = [pscustomobject]@{ Name = $Name }
    $certificate | Add-Member -MemberType ScriptMethod -Name GetNameInfo -Value {
        param($type, $issuer) $this.Name
    }
    return $certificate
}
function Test-SignaturePolicy([string]$Name, [bool]$Required, [string]$Status,
    $Signer, $Timestamp, [string]$ExpectedError = '') {
    $script:RequireSignature = $Required
    $script:signature = [pscustomobject]@{
        Status = [System.Management.Automation.SignatureStatus]::$Status
        SignerCertificate = $Signer
        TimeStamperCertificate = $Timestamp
    }
    $failure = ''
    try { Assert-Signature 'fixture.dll' | Out-Null }
    catch { $failure = $_.Exception.Message }
    if ($ExpectedError) {
        if ($failure -notlike "*$ExpectedError*") {
            throw "$Name expected '$ExpectedError', received '$failure'"
        }
    } elseif ($failure) { throw "$Name failed: $failure" }
    Write-Output "PASS: $Name"
}
$foundation = New-TestCertificate 'SignPath Foundation'
$other = New-TestCertificate 'Other Publisher'
$lookalike = New-TestCertificate 'SignPath Foundation Impostor'
$timestamp = [pscustomobject]@{}
Test-SignaturePolicy 'unsigned run' $false 'NotSigned' $null $null
Test-SignaturePolicy 'unsigned mode does not require Foundation' $false 'Valid' $other $null
Test-SignaturePolicy 'valid timestamped Foundation' $true 'Valid' $foundation $timestamp
Test-SignaturePolicy 'unsigned rejected' $true 'NotSigned' $null $null 'signature is NotSigned'
Test-SignaturePolicy 'invalid rejected' $true 'HashMismatch' $foundation $timestamp 'signature is HashMismatch'
Test-SignaturePolicy 'other publisher rejected' $true 'Valid' $other $timestamp 'subject is not SignPath Foundation'
Test-SignaturePolicy 'lookalike rejected' $true 'Valid' $lookalike $timestamp 'subject is not SignPath Foundation'
Test-SignaturePolicy 'missing signer rejected' $true 'Valid' $null $timestamp 'subject is not SignPath Foundation'
Test-SignaturePolicy 'missing timestamp rejected' $true 'Valid' $foundation $null 'no timestamp countersignature'
