# Kafka CLI - 集成测试运行脚本
# 自动启动 Kafka Docker 环境并运行测试

param(
    [switch]$StopKafka,
    [switch]$CleanUp
)

$VCPKG_ROOT = "C:\Users\lihongjie\vcpkg"

Write-Host "=== Kafka CLI Integration Test Runner ===" -ForegroundColor Cyan

if ($StopKafka) {
    Write-Host "`nStopping Kafka containers..." -ForegroundColor Yellow
    Push-Location docker\single-node
    docker-compose down
    if ($CleanUp) {
        docker-compose down -v
        Write-Host "Kafka data volumes removed" -ForegroundColor Green
    }
    Pop-Location
    exit 0
}

# 检查 Docker 是否运行
$dockerRunning = docker ps 2>$null
if ($LASTEXITCODE -ne 0) {
    Write-Host "Error: Docker is not running. Please start Docker first." -ForegroundColor Red
    exit 1
}

# 启动 Kafka
Write-Host "`n1. Starting Kafka cluster..." -ForegroundColor Green
Push-Location docker\single-node

# 清理旧容器
docker rm -f kafka kafka-ui 2>$null | Out-Null

# 启动新容器
docker-compose up -d

if ($LASTEXITCODE -ne 0) {
    Write-Host "Failed to start Kafka containers" -ForegroundColor Red
    Pop-Location
    exit 1
}

Write-Host "Waiting for Kafka to be ready (20 seconds)..." -ForegroundColor Yellow
Start-Sleep -Seconds 20

Pop-Location

# 检查 Kafka 是否就绪
Write-Host "`n2. Checking Kafka status..." -ForegroundColor Green
$kafkaStatus = docker ps --filter "name=kafka" --format "{{.Status}}"
if ($kafkaStatus -match "Up") {
    Write-Host "Kafka is running: $kafkaStatus" -ForegroundColor Green
} else {
    Write-Host "Warning: Kafka may not be ready yet" -ForegroundColor Yellow
}

# 设置环境变量
Write-Host "`n3. Setting up build environment..." -ForegroundColor Green
$env:PATH = "$VCPKG_ROOT;$VCPKG_ROOT\installed\x64-windows\tools\pkgconf;$VCPKG_ROOT\installed\x64-windows\bin;C:\Program Files\CMake\bin;" + $env:PATH
$env:PKG_CONFIG_PATH = "$VCPKG_ROOT\installed\x64-windows\lib\pkgconfig"
$env:RUST_LOG = "info"

# 运行测试
Write-Host "`n4. Running integration tests..." -ForegroundColor Green
Write-Host "   (This may take 40-50 seconds)`n" -ForegroundColor Yellow

cargo test --test integration_test -- --nocapture --test-threads=1

$testExitCode = $LASTEXITCODE

# 测试结果
Write-Host "`n=== Test Results ===" -ForegroundColor Cyan
if ($testExitCode -eq 0) {
    Write-Host "✅ All tests passed!" -ForegroundColor Green
    Write-Host "`nKafka cluster is still running. To stop it, run:" -ForegroundColor Yellow
    Write-Host "  .\run-tests.ps1 -StopKafka" -ForegroundColor Cyan
    Write-Host "`nTo stop and remove data:" -ForegroundColor Yellow
    Write-Host "  .\run-tests.ps1 -StopKafka -CleanUp" -ForegroundColor Cyan
} else {
    Write-Host "❌ Some tests failed. Check output above." -ForegroundColor Red
    Write-Host "`nKafka logs:" -ForegroundColor Yellow
    Write-Host "  docker logs kafka" -ForegroundColor Cyan
}

Write-Host "`nKafka UI: http://localhost:8080" -ForegroundColor Cyan

exit $testExitCode
