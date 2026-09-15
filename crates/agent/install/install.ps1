<#
.SYNOPSIS
    Installs the DumbMonit system agent and its Windows service.

.DESCRIPTION
    Downloads the binary, writes the configuration with the token, registers the
    service and starts it. Running the script again updates the agent: it is also
    the upgrade procedure.

.EXAMPLE
    & ([scriptblock]::Create((irm http://serveur:8080/install.ps1))) -Token ezym_xxx -Url http://serveur:8080

.EXAMPLE
    .\install.ps1 -Token ezym_xxx -Url http://serveur:8080 -Services @('Spooler','MSSQLSERVER')
#>
[CmdletBinding()]
param(
    [string]$Token,
    [string]$Url,
    [int]$Interval = 0,
    [string[]]$Services = @(),
    [hashtable]$Tags = @{},
    [string]$HostName,
    # Local binary to install instead of downloading it.
    [string]$BinPath,
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'

$ServiceName = 'EzyMonitAgent'
$InstallDir  = Join-Path $env:ProgramFiles 'EzyMonit'
$ConfigDir   = Join-Path $env:ProgramData 'EzyMonit'
$ExePath     = Join-Path $InstallDir 'ezymonit-agent.exe'
$ConfigPath  = Join-Path $ConfigDir 'agent.yaml'

function Write-Etape($message) { Write-Host "==> $message" }

function Stop-Sur($message) {
    Write-Error $message
    exit 1
}

# --------------------------------------------------------------- privilèges

$identite = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identite)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Stop-Sur "This installer needs a PowerShell console running as administrator."
}

# ------------------------------------------------------------ désinstallation

if ($Uninstall) {
    Write-Etape "Stopping the service"
    if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) {
        Stop-Service -Name $ServiceName -Force -ErrorAction SilentlyContinue
        # `sc.exe delete` plutôt que `Remove-Service` : ce dernier n'existe qu'à
        # partir de PowerShell 6, absent de bien des Windows Server encore en place.
        & sc.exe delete $ServiceName | Out-Null
    }
    Remove-Item -Path $InstallDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -Path $ConfigDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Etape "Agent uninstalled"
    exit 0
}

if ([string]::IsNullOrWhiteSpace($Token)) { Stop-Sur "The enrollment token is required (-Token)." }
if ([string]::IsNullOrWhiteSpace($Url))   { Stop-Sur "The server URL is required (-Url http://server:8080)." }
$Url = $Url.TrimEnd('/')

# ------------------------------------------------------------------ binaire

$architecture = switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { 'x86_64' }
    'ARM64' { 'aarch64' }
    default { Stop-Sur "Unsupported architecture: $env:PROCESSOR_ARCHITECTURE" }
}

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
New-Item -ItemType Directory -Path $ConfigDir  -Force | Out-Null

# Écriture à côté puis renommage : Windows verrouille le fichier d'un exécutable
# en cours, et un service déjà installé tiendrait le sien.
$exeTemporaire = "$ExePath.nouveau"

if ($BinPath) {
    if (-not (Test-Path $BinPath)) { Stop-Sur "Binary not found: $BinPath" }
    Write-Etape "Installing from $BinPath"
    Copy-Item -Path $BinPath -Destination $exeTemporaire -Force
} else {
    $source = "$Url/download/ezymonit-agent-windows-$architecture.exe"
    Write-Etape "Downloading $source"
    try {
        # Sans la barre de progression, `Invoke-WebRequest` est plusieurs fois plus
        # rapide sur Windows PowerShell 5 : elle repeint la console à chaque bloc.
        $progression = $ProgressPreference
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri $source -OutFile $exeTemporaire -UseBasicParsing
    } catch {
        Stop-Sur "Download failed from ${source}: $_"
    } finally {
        $ProgressPreference = $progression
    }
}

if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) {
    Write-Etape "Stopping the existing service before replacing it"
    Stop-Service -Name $ServiceName -Force -ErrorAction SilentlyContinue
}
Move-Item -Path $exeTemporaire -Destination $ExePath -Force

# ------------------------------------------------------------ configuration

$lignes = New-Object System.Collections.Generic.List[string]
$lignes.Add("# DumbMonit system agent configuration.")
$lignes.Add("# Written by install.ps1 — environment variables override it.")
$lignes.Add("server_url: $Url")
$lignes.Add("token: $Token")
if ($Interval -gt 0)  { $lignes.Add("interval_secs: $Interval") }
if ($HostName)        { $lignes.Add("hostname: $HostName") }
if ($Services.Count -gt 0) {
    $lignes.Add("services:")
    foreach ($service in $Services) { $lignes.Add("  - $service") }
}
if ($Tags.Count -gt 0) {
    $lignes.Add("tags:")
    foreach ($cle in $Tags.Keys) { $lignes.Add("  ${cle}: $($Tags[$cle])") }
}

# UTF-8 sans marque d'ordre : l'analyseur YAML de l'agent lit de l'UTF-8 brut, et
# une marque d'ordre en tête ferait échouer la première clé.
$encodage = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllLines($ConfigPath, $lignes, $encodage)

# Le fichier porte le jeton : seuls le système et les administrateurs doivent le
# lire. L'héritage est coupé, sinon les droits du dossier parent le rouvriraient.
& icacls.exe $ConfigPath /inheritance:r /grant:r 'SYSTEM:(R)' 'Administrators:(F)' | Out-Null
Write-Etape "Configuration written to $ConfigPath"

# --------------------------------------------------------------- vérification

# Un envoi de test avant de démarrer le service : un jeton erroné ou un serveur
# injoignable doit se voir maintenant, pas dans le journal des événements.
Write-Etape "Checking the connection to $Url"
& $ExePath --config="$ConfigPath" --once
if ($LASTEXITCODE -ne 0) {
    Stop-Sur @"
The agent could not reach the server — check the URL and the token.
The configuration is in place: fix $ConfigPath, then run
'Start-Service $ServiceName'.
"@
}

# ------------------------------------------------------------------- service

$commande = "`"$ExePath`" --service --config=`"$ConfigPath`""

if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) {
    Write-Etape "Updating the existing service"
    & sc.exe config $ServiceName binPath= $commande start= auto | Out-Null
} else {
    Write-Etape "Registering the service"
    New-Service -Name $ServiceName `
        -BinaryPathName $commande `
        -DisplayName "DumbMonit system agent" `
        -Description "Collects this machine's metrics and pushes them to the DumbMonit server." `
        -StartupType Automatic | Out-Null
}

# Redémarrage automatique après une panne : sans cela, un agent tombé une fois ne
# revient qu'au prochain redémarrage de la machine — soit jamais, sur un serveur.
& sc.exe failure $ServiceName reset= 86400 actions= restart/10000/restart/30000/restart/60000 | Out-Null

Start-Service -Name $ServiceName

Write-Etape "Agent installed and started"
Write-Etape "Status: Get-Service $ServiceName"
Write-Etape "Logs:   Get-EventLog -LogName Application -Source $ServiceName -Newest 20"
