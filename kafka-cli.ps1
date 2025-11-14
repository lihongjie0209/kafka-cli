# Kafka CLI - 快速启动脚本
# 设置必要的环境变量并运行 kafka-cli

# 设置 vcpkg 路径（根据你的安装位置调整）
$VCPKG_ROOT = "C:\Users\lihongjie\vcpkg"

# 添加必要的路径到 PATH
$env:PATH = "$VCPKG_ROOT;$VCPKG_ROOT\installed\x64-windows\tools\pkgconf;$VCPKG_ROOT\installed\x64-windows\bin;C:\Program Files\CMake\bin;" + $env:PATH

# 设置 PKG_CONFIG_PATH
$env:PKG_CONFIG_PATH = "$VCPKG_ROOT\installed\x64-windows\lib\pkgconfig"

# 运行 kafka-cli，传递所有参数
& ".\target\release\kafka-cli.exe" $args
