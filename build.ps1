# Kafka CLI - 构建脚本
# 设置必要的环境变量并构建项目

# 设置 vcpkg 路径（根据你的安装位置调整）
$VCPKG_ROOT = "C:\Users\lihongjie\vcpkg"

Write-Host "Setting up build environment..." -ForegroundColor Green

# 添加必要的路径到 PATH
$env:PATH = "$VCPKG_ROOT;$VCPKG_ROOT\installed\x64-windows\tools\pkgconf;$VCPKG_ROOT\installed\x64-windows\bin;C:\Program Files\CMake\bin;" + $env:PATH

# 设置 PKG_CONFIG_PATH
$env:PKG_CONFIG_PATH = "$VCPKG_ROOT\installed\x64-windows\lib\pkgconfig"

Write-Host "Building kafka-cli..." -ForegroundColor Green

# 根据参数选择构建类型
if ($args[0] -eq "debug") {
    cargo build
} else {
    cargo build --release
}

if ($LASTEXITCODE -eq 0) {
    Write-Host "`nBuild successful! 🎉" -ForegroundColor Green
    if ($args[0] -eq "debug") {
        Write-Host "Binary location: .\target\debug\kafka-cli.exe" -ForegroundColor Cyan
    } else {
        Write-Host "Binary location: .\target\release\kafka-cli.exe" -ForegroundColor Cyan
    }
    Write-Host "`nRun with: .\target\release\kafka-cli.exe --help" -ForegroundColor Yellow
    Write-Host "Or use the helper script: .\kafka-cli.ps1 --help" -ForegroundColor Yellow
} else {
    Write-Host "`nBuild failed. Please check the error messages above." -ForegroundColor Red
}
