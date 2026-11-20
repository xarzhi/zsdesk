<#
.SYNOPSIS
    按指定版本号构建 ZSDesk 的 Windows x64 安装包（setup.exe）。

.DESCRIPTION
    一条命令完成：编译 → 校验版本 → 归集文件 → 调用 Inno Setup 生成安装包。

        releases\<版本>\ZSDesk-<版本>-x64-setup.exe

    给出的版本号会临时写进 Cargo.toml 和资源版本信息（exe 属性、关于对话框、
    卸载列表都取这一份），编译完成立刻还原，工作区不会留下改动。

.PARAMETER Version
    版本号，形如 0.1.0 或 0.3.6。省略时使用 Cargo.toml 里的当前版本。

.PARAMETER SkipBuild
    跳过编译，直接用 target\release\zsdesk-app.exe 重新打包。
    这个 exe 的版本必须与请求的版本一致，否则脚本会拒绝打包。

.PARAMETER EverythingDir
    可选。Everything 官方运行文件目录（含 Everything.exe、manifest.json）。
    给出后随包附带搜索引擎，装在 engines\everything 下；不给则跳过，
    程序会改用系统里已安装的 Everything。

.PARAMETER ISCCPath
    可选。Inno Setup 的 ISCC.exe 路径；默认在常见的安装位置里查找。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\package.ps1 0.1.0

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\package.ps1
    不带参数：按 Cargo.toml 里的当前版本打包。
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)][string]$Version,
    [switch]$SkipBuild,
    [string]$EverythingDir = $env:ZSDESK_EVERYTHING_DIR,
    [string]$ISCCPath = $env:ZSDESK_ISCC
)

$ErrorActionPreference = 'Stop'

$root = if ($PSScriptRoot) { $PSScriptRoot } else { Split-Path -Parent $MyInvocation.MyCommand.Path }
$cargoToml = Join-Path $root 'Cargo.toml'
$rcPath = Join-Path $root 'crates\zsdesk-app\resources\zsdesk.rc'
$issPath = Join-Path $root 'packaging\windows\setup.iss'
$noticePath = Join-Path $root 'THIRD-PARTY-NOTICES.txt'
$licensePath = Join-Path $root 'LICENSE'

foreach ($file in @($cargoToml, $rcPath, $issPath, $licensePath)) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "缺少必需文件：$file" }
}

function Read-TextFile([string]$Path) {
    return [IO.File]::ReadAllText($Path, [Text.Encoding]::UTF8)
}

function Write-TextFile([string]$Path, [string]$Text, [bool]$Bom = $false) {
    $encoding = New-Object Text.UTF8Encoding($Bom)
    [IO.File]::WriteAllText($Path, $Text, $encoding)
}

function Find-InnoCompiler {
    if ($ISCCPath) {
        if (-not (Test-Path -LiteralPath $ISCCPath -PathType Leaf)) { throw "ISCC.exe 不存在：$ISCCPath" }
        return $ISCCPath
    }
    $candidates = @()
    $command = Get-Command 'ISCC.exe' -ErrorAction SilentlyContinue
    if ($command) { $candidates += $command.Source }
    $candidates += @(
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe')
    )
    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) { return $candidate }
    }
    throw '未找到 Inno Setup 6 的 ISCC.exe；请安装 Inno Setup 6，或用 -ISCCPath 指定路径。'
}

# 可选的 Visual C++ 运行库：找得到就一起装，找不到不阻断打包。
function Find-RuntimeDir {
    $roots = @()
    if ($env:VCToolsRedistDir) { $roots += (Join-Path $env:VCToolsRedistDir 'x64') }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path -LiteralPath $vswhere -PathType Leaf) {
        $installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if ($installation) {
            $redist = Join-Path $installation 'VC\Redist\MSVC'
            if (Test-Path -LiteralPath $redist -PathType Container) {
                $versions = Get-ChildItem -LiteralPath $redist -Directory |
                    Where-Object { $_.Name -match '^\d+\.\d+\.\d+$' } |
                    Sort-Object { [version]$_.Name } -Descending
                foreach ($item in $versions) { $roots += (Join-Path $item.FullName 'x64') }
            }
        }
    }
    foreach ($candidate in $roots) {
        if (-not (Test-Path -LiteralPath $candidate -PathType Container)) { continue }
        $crt = Get-ChildItem -LiteralPath $candidate -Directory -Filter 'Microsoft.VC*.CRT' -ErrorAction SilentlyContinue |
            Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'msvcp140.dll') } |
            Select-Object -First 1
        if ($crt) { return $crt.FullName }
    }
    return $null
}

# 读取 exe 自报的身份。必须显式按 UTF-8 解码：Windows PowerShell 默认用 OEM
# 代码页读取子进程输出，中文版本名会被读成乱码，JSON 也就解析不了。
function Get-BuildInfo([string]$Path) {
    $start = New-Object Diagnostics.ProcessStartInfo
    $start.FileName = $Path
    $start.Arguments = '--build-info'
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = New-Object Text.UTF8Encoding($false)
    $start.StandardErrorEncoding = New-Object Text.UTF8Encoding($false)
    $process = [Diagnostics.Process]::Start($start)
    try {
        $stdout = $process.StandardOutput.ReadToEnd()
        $stderr = $process.StandardError.ReadToEnd()
        if (-not $process.WaitForExit(15000)) { $process.Kill(); throw '查询产物身份超时' }
        if ($process.ExitCode -ne 0) { throw "查询产物身份失败：$stderr" }
        return ($stdout | ConvertFrom-Json)
    } finally {
        $process.Dispose()
    }
}

$tomlText = Read-TextFile $cargoToml
$workspaceVersion = [regex]::Match($tomlText, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $workspaceVersion) { throw 'Cargo.toml 里找不到 workspace 版本号' }

if (-not $Version) { $Version = $workspaceVersion }
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "版本号必须是 x.y.z 形式（例如 0.1.0），收到：$Version" }
$numbers = $Version -split '\.'
$quad = '{0},{1},{2},0' -f $numbers[0], $numbers[1], $numbers[2]

$iscc = Find-InnoCompiler
Write-Host "==> ZSDesk $Version（工作区当前版本 $workspaceVersion）"
Write-Host "    Inno Setup: $iscc"

# ---------------------------------------------------------------- 编译
$originalToml = $tomlText
$originalRc = Read-TextFile $rcPath
$lockPath = Join-Path $root 'Cargo.lock'
$originalLock = if (Test-Path -LiteralPath $lockPath -PathType Leaf) { Read-TextFile $lockPath } else { $null }
$patched = $false

try {
    if (-not $SkipBuild) {
        if ($Version -ne $workspaceVersion) {
            $patched = $true
            Write-Host '    临时写入版本号到 Cargo.toml 与资源脚本'
            $versionLine = New-Object System.Text.RegularExpressions.Regex('(?m)^version\s*=\s*"[^"]+"')
            Write-TextFile $cargoToml ($versionLine.Replace($originalToml, "version = `"$Version`"", 1))
            $rc = $originalRc
            $rc = [regex]::Replace($rc, 'FILEVERSION(\s+)[\d,]+', ('FILEVERSION${1}' + $quad))
            $rc = [regex]::Replace($rc, 'PRODUCTVERSION(\s+)[\d,]+', ('PRODUCTVERSION${1}' + $quad))
            $rc = [regex]::Replace($rc, '"FileVersion",(\s*)"[^"]*"', ('"FileVersion",${1}"' + $Version + '\0"'))
            $rc = [regex]::Replace($rc, '"ProductVersion",(\s*)"[^"]*"', ('"ProductVersion",${1}"' + $Version + '\0"'))
            Write-TextFile $rcPath $rc
        }
        Write-Host '    编译中（cargo build --release -p zsdesk-app）…'
        Push-Location $root
        try {
            & cargo build --release -p zsdesk-app
            if ($LASTEXITCODE -ne 0) { throw "cargo build 失败（退出码 $LASTEXITCODE）" }
        } finally {
            Pop-Location
        }
    }
} finally {
    if ($patched) {
        Write-TextFile $cargoToml $originalToml
        Write-TextFile $rcPath $originalRc
        if ($null -ne $originalLock) { Write-TextFile $lockPath $originalLock }
        Push-Location $root
        try { & cargo metadata --format-version 1 --no-deps 2>&1 | Out-Null } finally { Pop-Location }
        Write-Host '    已还原 Cargo.toml 与资源版本信息'
    }
}

# ---------------------------------------------------------------- 校验产物
$exe = Join-Path $root 'target\release\zsdesk-app.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { throw "找不到编译产物：$exe" }

$identity = Get-BuildInfo $exe
if ($identity.product -ne 'ZSDesk') { throw "产物不是 ZSDesk：$($identity.product)" }
if ($identity.version -ne $Version) { throw "产物版本是 $($identity.version)，与请求的 $Version 不一致（-SkipBuild 时请先重新编译）" }

$fileVersion = (Get-Item -LiteralPath $exe).VersionInfo.ProductVersion
if ($fileVersion -ne $Version) { throw "exe 文件属性版本是 $fileVersion，与请求的 $Version 不一致" }
Write-Host "    产物校验通过：$($identity.edition)/$($identity.version)"

# ---------------------------------------------------------------- 归集文件
$payload = Join-Path $root "build\package\$Version\payload"
if (Test-Path -LiteralPath $payload) { Remove-Item -LiteralPath $payload -Recurse -Force }
New-Item -ItemType Directory -Path $payload -Force | Out-Null

Copy-Item -LiteralPath $exe -Destination (Join-Path $payload 'ZSDesk.exe')
Copy-Item -LiteralPath $licensePath -Destination (Join-Path $payload 'LICENSE.txt')
if (Test-Path -LiteralPath $noticePath -PathType Leaf) {
    Copy-Item -LiteralPath $noticePath -Destination (Join-Path $payload 'THIRD-PARTY-NOTICES.txt')
}

$runtimeDir = Find-RuntimeDir
if ($runtimeDir) {
    foreach ($name in @('msvcp140.dll', 'vcruntime140.dll', 'vcruntime140_1.dll')) {
        $source = Join-Path $runtimeDir $name
        if (Test-Path -LiteralPath $source -PathType Leaf) { Copy-Item -LiteralPath $source -Destination (Join-Path $payload $name) }
    }
    Write-Host "    附带 Visual C++ 运行库：$runtimeDir"
} else {
    Write-Warning '未找到 Visual C++ 运行库目录，安装包不附带运行库；目标机需自行安装 VC++ 2015-2022 x64。'
}

if ($EverythingDir) {
    $engineSource = [IO.Path]::GetFullPath($EverythingDir)
    foreach ($name in @('Everything.exe', 'Everything.lng', 'License.txt', 'manifest.json')) {
        if (-not (Test-Path -LiteralPath (Join-Path $engineSource $name) -PathType Leaf)) { throw "Everything 目录缺少 $name：$engineSource" }
    }
    $pinned = Join-Path $root 'packaging\engines\everything.json'
    if ((Get-FileHash -LiteralPath (Join-Path $engineSource 'manifest.json')).Hash -ne (Get-FileHash -LiteralPath $pinned).Hash) {
        throw 'Everything 目录的 manifest.json 与 packaging\engines\everything.json 不一致'
    }
    $engineTarget = Join-Path $payload 'engines\everything'
    New-Item -ItemType Directory -Path $engineTarget -Force | Out-Null
    foreach ($name in @('Everything.exe', 'Everything.lng', 'License.txt', 'manifest.json')) {
        Copy-Item -LiteralPath (Join-Path $engineSource $name) -Destination (Join-Path $engineTarget $name)
    }
    Write-Host "    附带 Everything 引擎：$engineSource"
} else {
    Write-Host '    未附带 Everything 引擎（程序会复用系统里已安装的 Everything）'
}

$readme = @(
    "ZSDesk $Version — Windows x64",
    '',
    'ZSDesk.exe 直接运行；配置保存在 %APPDATA%\ZSDesk。',
    '卸载不会删除个人配置与搜索索引。',
    '',
    '源码与许可证：https://github.com/qiu7824/zsdesk'
) -join "`r`n"
Write-TextFile (Join-Path $payload '说明.txt') $readme $true

$files = @(Get-ChildItem -LiteralPath $payload -File -Recurse)
Write-Host "    归集完成：$($files.Count) 个文件，$('{0:N1}' -f (($files | Measure-Object -Property Length -Sum).Sum / 1MB)) MB"

# ---------------------------------------------------------------- 生成安装包
$outputDir = Join-Path $root "releases\$Version"
New-Item -ItemType Directory -Path $outputDir -Force | Out-Null
$setupPath = Join-Path $outputDir "ZSDesk-$Version-x64-setup.exe"
if (Test-Path -LiteralPath $setupPath -PathType Leaf) { Remove-Item -LiteralPath $setupPath -Force }

Write-Host '    调用 Inno Setup 生成安装包…'
& $iscc '/Qp' "/DSourceDir=$payload" "/DOutputDir=$outputDir" "/DMyAppVersion=$Version" $issPath
if ($LASTEXITCODE -ne 0) { throw "Inno Setup 编译失败（退出码 $LASTEXITCODE）" }
if (-not (Test-Path -LiteralPath $setupPath -PathType Leaf)) { throw "安装包没有生成：$setupPath" }

$setup = Get-Item -LiteralPath $setupPath
$hash = (Get-FileHash -LiteralPath $setupPath -Algorithm SHA256).Hash.ToLowerInvariant()

Write-Host ''
Write-Host '打包完成'
Write-Host "  安装包：$($setup.FullName)"
Write-Host ("  大小：  {0:N1} MB" -f ($setup.Length / 1MB))
Write-Host "  SHA256：$hash"
Write-Host "  版本：  $($setup.VersionInfo.ProductVersion) / $($setup.VersionInfo.FileDescription)"
Write-Host ''
