# 起本地 Redis（单机 + 3 主 3 从集群）并跑那些 #[ignore] 的端到端测试。
#
#   ./scripts/dev/e2e.ps1          起环境、跑测试、留着环境
#   ./scripts/dev/e2e.ps1 -Down    只拆环境
#
# 这些测试默认是 ignore 的，因为它们需要一个可写的真实 Redis。别去掉 ignore：
# `npm run check` 必须在没有 Redis 的机器上也能跑。
#
# 为什么要有这个脚本：单元测试喂的是手工构造的 redis::Value，而真正会出错的
# 地方是「真实服务端回什么形状」——分页端点、集群路由、字节保真。这些东西
# 单元测试一条都发现不了。
param([switch]$Down)

$ErrorActionPreference = 'Stop'
$Standalone = 6399
$AuthPort = 6398
$LegacyPort = 6397
# 能力探测矩阵（tests/redis_capabilities.rs）：身份字段与能力结论只能对着真实服务端定
$Matrix = [ordered]@{
    'tsubasa-redis5'    = @{ Port = 6396; Image = 'redis:5-alpine';                                    Cli = 'redis-cli';  Args = @('redis-server', '--save', '', '--appendonly', 'no') }
    'tsubasa-valkey8'   = @{ Port = 6395; Image = 'valkey/valkey:8-alpine';                            Cli = 'valkey-cli'; Args = @('valkey-server', '--save', '', '--appendonly', 'no') }
    'tsubasa-valkey9'   = @{ Port = 6394; Image = 'valkey/valkey:9-alpine';                            Cli = 'valkey-cli'; Args = @('valkey-server', '--save', '', '--appendonly', 'no') }
    'tsubasa-keydb'     = @{ Port = 6393; Image = 'eqalpha/keydb:latest';                              Cli = 'keydb-cli';  Args = @('keydb-server', '--save', '', '--appendonly', 'no') }
    'tsubasa-dragonfly' = @{ Port = 6392; Image = 'docker.dragonflydb.io/dragonflydb/dragonfly:latest'; Cli = 'redis-cli';  Args = @() }
}
$AuthPassword = 'correct-horse'
$ClusterSeed = 7000
$Image = 'redis:7-alpine'
$Root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

function Remove-Env {
    docker rm -f tsubasa-local 2>$null | Out-Null
    docker rm -f tsubasa-cluster 2>$null | Out-Null
    docker rm -f tsubasa-auth 2>$null | Out-Null
    docker rm -f tsubasa-legacy 2>$null | Out-Null
    foreach ($name in $Matrix.Keys) { docker rm -f $name 2>$null | Out-Null }
}

if ($Down) { Remove-Env; Write-Host '环境已拆除'; exit 0 }

Remove-Env

# 一律绑 127.0.0.1：无认证的 Redis 绝不能监听在对外地址上。
Write-Host "起单机 Redis (127.0.0.1:$Standalone)…"
docker run -d --name tsubasa-local -p "127.0.0.1:${Standalone}:6379" $Image `
    redis-server --save '' --appendonly no | Out-Null

# 带密码的实例：用来验证凭据错误是**快速**失败而不是重试到超时
Write-Host "起带密码的 Redis (127.0.0.1:$AuthPort)…"
docker run -d --name tsubasa-auth -p "127.0.0.1:${AuthPort}:6379" $Image `
    redis-server --save '' --appendonly no --requirepass $AuthPassword | Out-Null

Write-Host "起集群 (127.0.0.1:$ClusterSeed …7005)…"
Write-Host "起 Redis 6 兼容性实例 (127.0.0.1:$LegacyPort)…"
docker run -d --name tsubasa-legacy -p "127.0.0.1:${LegacyPort}:6379" redis:6-alpine `
    redis-server --save '' --appendonly no | Out-Null
$ports = @()
7000..7005 | ForEach-Object { $ports += '-p'; $ports += "127.0.0.1:$($_):$($_)" }
docker run -d --name tsubasa-cluster @ports `
    -v "$Root/scripts/dev/redis-cluster.sh:/init.sh:ro" $Image sh /init.sh | Out-Null

foreach ($name in $Matrix.Keys) {
    $spec = $Matrix[$name]
    Write-Host "起 $($spec.Image) (127.0.0.1:$($spec.Port))…"
    $serverArgs = $spec.Args
    docker run -d --name $name -p "127.0.0.1:$($spec.Port):6379" $spec.Image @serverArgs | Out-Null
}

# 等两边真的可用再跑，不要靠 sleep 猜
$deadline = (Get-Date).AddSeconds(60)
while ((Get-Date) -lt $deadline) {
    $ok = $false
    try {
        $a = docker exec tsubasa-local redis-cli ping 2>$null
        $b = docker exec tsubasa-cluster redis-cli -p $ClusterSeed cluster info 2>$null
        $c = docker exec tsubasa-auth redis-cli -a $AuthPassword ping 2>$null
        $d = docker exec tsubasa-legacy redis-cli ping 2>$null
        $ok = ($a -match 'PONG') -and ($b -match 'cluster_state:ok') -and ($c -match 'PONG') -and ($d -match 'PONG')
        foreach ($name in $Matrix.Keys) {
            $pong = docker exec $name $Matrix[$name].Cli ping 2>$null
            $ok = $ok -and ($pong -match 'PONG')
        }
    } catch { $ok = $false }
    if ($ok) { break }
    Start-Sleep -Milliseconds 500
}
if (-not $ok) { throw '环境没能在 60 秒内就绪' }

$env:TEST_REDIS_HOST = '127.0.0.1';    $env:TEST_REDIS_PORT = "$Standalone"
$env:TEST_REDIS_AUTH_PORT = "$AuthPort"
$env:TEST_REDIS_LEGACY_PORT = "$LegacyPort"
$env:TEST_REDIS_5_PORT = "$($Matrix['tsubasa-redis5'].Port)"
$env:TEST_VALKEY_8_PORT = "$($Matrix['tsubasa-valkey8'].Port)"
$env:TEST_VALKEY_9_PORT = "$($Matrix['tsubasa-valkey9'].Port)"
$env:TEST_KEYDB_PORT = "$($Matrix['tsubasa-keydb'].Port)"
$env:TEST_DRAGONFLY_PORT = "$($Matrix['tsubasa-dragonfly'].Port)"
$env:PHASE3A_REDIS_HOST = '127.0.0.1'; $env:PHASE3A_REDIS_PORT = "$Standalone"
$env:PHASE3B_REDIS_HOST = '127.0.0.1'; $env:PHASE3B_REDIS_PORT = "$Standalone"
$env:PHASE4C_REDIS_HOST = '127.0.0.1'; $env:PHASE4C_REDIS_PORT = "$Standalone"
$env:PHASE2_HOST = '127.0.0.1'
$env:PHASE2_STANDALONE_PORT = "$Standalone"
$env:PHASE2_CLUSTER_PORT = "$ClusterSeed"

$failed = @()
foreach ($t in @('redis_basic', 'redis_values', 'redis_key_write', 'redis_phase3a', 'redis_phase3b', 'redis_phase4c', 'redis_phase5a', 'redis_phase5b', 'redis_capabilities')) {
    Write-Host "`n--- $t ---"
    cargo test --manifest-path "$Root/src-tauri/Cargo.toml" --test $t -- --ignored
    if ($LASTEXITCODE -ne 0) { $failed += $t }
}

# phase2 里 TLS / SSH / 哨兵还需要更多环境，这里只跑集群那几条
Write-Host "`n--- redis_phase2 (cluster) ---"
cargo test --manifest-path "$Root/src-tauri/Cargo.toml" --test redis_phase2 -- --ignored redis_cluster
if ($LASTEXITCODE -ne 0) { $failed += 'redis_phase2' }

if ($failed.Count -gt 0) {
    Write-Host "`n失败: $($failed -join ', ')" -ForegroundColor Red
    exit 1
}
Write-Host "`n全部通过。拆环境: ./scripts/dev/e2e.ps1 -Down" -ForegroundColor Green
