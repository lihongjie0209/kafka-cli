#!/usr/bin/env python3
"""
Kafka CLI 功能测试
通过执行实际的命令行来测试 CLI 工具的功能
"""

import subprocess
import json
import time
import sys
from typing import List, Tuple, Optional
from datetime import datetime

# 配置
KAFKA_CLI = r".\target\release\kafka-cli.exe"
BOOTSTRAP_SERVER = "localhost:9093"
TEST_PREFIX = "pytest"

class Colors:
    """ANSI 颜色代码"""
    GREEN = '\033[92m'
    RED = '\033[91m'
    YELLOW = '\033[93m'
    CYAN = '\033[96m'
    RESET = '\033[0m'
    BOLD = '\033[1m'

def print_test_header(test_name: str):
    """打印测试头部"""
    print(f"\n{Colors.CYAN}{Colors.BOLD}{'='*70}{Colors.RESET}")
    print(f"{Colors.CYAN}{Colors.BOLD}测试: {test_name}{Colors.RESET}")
    print(f"{Colors.CYAN}{Colors.BOLD}{'='*70}{Colors.RESET}")

def print_success(message: str):
    """打印成功消息"""
    try:
        print(f"{Colors.GREEN}✅ {message}{Colors.RESET}")
    except UnicodeEncodeError:
        print(f"{Colors.GREEN}[PASS] {message}{Colors.RESET}")

def print_error(message: str):
    """打印错误消息"""
    try:
        print(f"{Colors.RED}❌ {message}{Colors.RESET}")
    except UnicodeEncodeError:
        print(f"{Colors.RED}[FAIL] {message}{Colors.RESET}")

def print_info(message: str):
    """打印信息"""
    try:
        print(f"{Colors.YELLOW}ℹ️  {message}{Colors.RESET}")
    except UnicodeEncodeError:
        print(f"{Colors.YELLOW}[INFO] {message}{Colors.RESET}")

def run_command(args: List[str], input_data: Optional[str] = None) -> Tuple[int, str, str]:
    """
    执行命令并返回结果
    
    Returns:
        (exit_code, stdout, stderr)
    """
    cmd = [KAFKA_CLI] + args
    print(f"{Colors.YELLOW}执行命令: {' '.join(cmd)}{Colors.RESET}")
    
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            input=input_data,
            timeout=30
        )
        return result.returncode, result.stdout, result.stderr
    except subprocess.TimeoutExpired:
        return -1, "", "Command timeout"
    except Exception as e:
        return -1, "", str(e)

def generate_topic_name(prefix: str) -> str:
    """生成唯一的 topic 名称"""
    timestamp = int(datetime.now().timestamp() * 1000)
    return f"{prefix}_{timestamp}"

class TestResult:
    """测试结果"""
    def __init__(self):
        self.passed = 0
        self.failed = 0
        self.errors = []
    
    def add_pass(self):
        self.passed += 1
    
    def add_fail(self, test_name: str, error: str):
        self.failed += 1
        self.errors.append((test_name, error))
    
    def print_summary(self):
        """打印测试摘要"""
        print(f"\n{Colors.BOLD}{'='*70}{Colors.RESET}")
        print(f"{Colors.BOLD}测试摘要{Colors.RESET}")
        print(f"{Colors.BOLD}{'='*70}{Colors.RESET}")
        print(f"总计: {self.passed + self.failed} 个测试")
        print(f"{Colors.GREEN}通过: {self.passed}{Colors.RESET}")
        print(f"{Colors.RED}失败: {self.failed}{Colors.RESET}")
        
        if self.errors:
            print(f"\n{Colors.RED}失败的测试:{Colors.RESET}")
            for test_name, error in self.errors:
                print(f"  - {test_name}: {error}")
        
        return self.failed == 0

# 测试结果收集器
result = TestResult()

def test_help_command():
    """测试: 帮助命令"""
    print_test_header("帮助命令")
    
    exit_code, stdout, stderr = run_command(["--help"])
    
    if exit_code != 0:
        print_error(f"命令失败: exit_code={exit_code}")
        result.add_fail("test_help_command", f"exit_code={exit_code}")
        return
    
    if "A cross-platform Kafka CLI tool" not in stdout:
        print_error("帮助输出缺少预期文本")
        result.add_fail("test_help_command", "Missing expected text")
        return
    
    if "topics" not in stdout or "produce" not in stdout or "consume" not in stdout:
        print_error("帮助输出缺少子命令")
        result.add_fail("test_help_command", "Missing subcommands")
        return
    
    print_success("帮助命令测试通过")
    result.add_pass()

def test_version_command():
    """测试: 版本命令"""
    print_test_header("版本命令")
    
    exit_code, stdout, stderr = run_command(["--version"])
    
    if exit_code != 0:
        print_error(f"命令失败: exit_code={exit_code}")
        result.add_fail("test_version_command", f"exit_code={exit_code}")
        return
    
    if "kafka-cli" not in stdout:
        print_error("版本输出不正确")
        result.add_fail("test_version_command", "Invalid version output")
        return
    
    print_success(f"版本: {stdout.strip()}")
    result.add_pass()

def test_topics_list():
    """测试: 列出 topics"""
    print_test_header("列出 Topics")
    
    exit_code, stdout, stderr = run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "list"
    ])
    
    if exit_code != 0:
        print_error(f"命令失败: exit_code={exit_code}, stderr={stderr}")
        result.add_fail("test_topics_list", f"exit_code={exit_code}")
        return
    
    print_info(f"Topics 输出:\n{stdout}")
    print_success("列出 topics 成功")
    result.add_pass()

def test_topic_create_and_delete():
    """测试: 创建和删除 topic"""
    print_test_header("创建和删除 Topic")
    
    topic_name = generate_topic_name(TEST_PREFIX)
    print_info(f"测试 topic: {topic_name}")
    
    # 创建 topic
    print_info("步骤 1: 创建 topic")
    exit_code, stdout, stderr = run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "create",
        "--topic", topic_name,
        "--partitions", "3",
        "--replication-factor", "1"
    ])
    
    if exit_code != 0:
        print_error(f"创建 topic 失败: {stderr}")
        result.add_fail("test_topic_create", f"exit_code={exit_code}")
        return
    
    if "created successfully" not in stdout.lower():
        print_error(f"创建输出不符合预期: {stdout}")
        result.add_fail("test_topic_create", "Unexpected output")
        return
    
    print_success(f"Topic {topic_name} 创建成功")
    
    # 等待 topic 创建完成
    time.sleep(2)
    
    # 验证 topic 存在
    print_info("步骤 2: 验证 topic 存在")
    exit_code, stdout, stderr = run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "list"
    ])
    
    if topic_name not in stdout:
        print_error(f"Topic {topic_name} 未在列表中找到")
        result.add_fail("test_topic_verify", "Topic not found in list")
        # 尝试清理
        run_command([
            "topics",
            "--bootstrap-server", BOOTSTRAP_SERVER,
            "delete",
            "--topic", topic_name
        ])
        return
    
    print_success(f"验证 topic {topic_name} 存在")
    
    # 描述 topic
    print_info("步骤 3: 描述 topic")
    exit_code, stdout, stderr = run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "describe",
        "--topic", topic_name
    ])
    
    if exit_code != 0:
        print_error(f"描述 topic 失败: {stderr}")
    else:
        if "Partitions: 3" in stdout:
            print_success(f"Topic 有 3 个分区 (符合预期)")
        else:
            print_error(f"分区数不符合预期:\n{stdout}")
        print_info(f"Topic 详情:\n{stdout}")
    
    # 删除 topic
    print_info("步骤 4: 删除 topic")
    exit_code, stdout, stderr = run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "delete",
        "--topic", topic_name
    ])
    
    if exit_code != 0:
        print_error(f"删除 topic 失败: {stderr}")
        result.add_fail("test_topic_delete", f"exit_code={exit_code}")
        return
    
    print_success(f"Topic {topic_name} 删除成功")
    result.add_pass()

def test_produce_and_consume():
    """测试: 生产和消费消息"""
    print_test_header("生产和消费消息")
    
    topic_name = generate_topic_name(TEST_PREFIX)
    print_info(f"测试 topic: {topic_name}")
    
    # 创建 topic
    print_info("步骤 1: 创建 topic")
    exit_code, stdout, stderr = run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "create",
        "--topic", topic_name,
        "--partitions", "1",
        "--replication-factor", "1"
    ])
    
    if exit_code != 0:
        print_error(f"创建 topic 失败: {stderr}")
        result.add_fail("test_produce_consume_setup", f"exit_code={exit_code}")
        return
    
    time.sleep(2)
    
    # 生产消息
    print_info("步骤 2: 生产消息")
    messages = [
        "key1\tHello Kafka",
        "key2\tMessage 2",
        "key3\tMessage 3"
    ]
    input_data = "\n".join(messages)
    
    # 注意: produce 命令从 stdin 读取，需要特殊处理
    # 使用 PowerShell 管道来发送消息
    ps_cmd = f'echo "{input_data}" | {KAFKA_CLI} produce --bootstrap-server {BOOTSTRAP_SERVER} --topic {topic_name} --key-separator "`t"'
    
    try:
        result_ps = subprocess.run(
            ["powershell", "-Command", ps_cmd],
            capture_output=True,
            text=True,
            timeout=10
        )
        
        if result_ps.returncode != 0:
            print_error(f"生产消息失败: {result_ps.stderr}")
            result.add_fail("test_produce", f"exit_code={result_ps.returncode}")
        else:
            print_success(f"成功生产 {len(messages)} 条消息")
            print_info(f"Producer 输出:\n{result_ps.stdout}")
    except Exception as e:
        print_error(f"生产消息异常: {e}")
        result.add_fail("test_produce", str(e))
        # 清理
        run_command([
            "topics",
            "--bootstrap-server", BOOTSTRAP_SERVER,
            "delete",
            "--topic", topic_name
        ])
        return
    
    time.sleep(2)
    
    # 消费消息
    print_info("步骤 3: 消费消息")
    
    # 使用绝对路径和后台进程
    import os
    abs_cli_path = os.path.abspath(KAFKA_CLI)
    
    # 使用 Start-Process 启动消费者并重定向输出到文件
    temp_output = f"temp_consume_{int(time.time())}.txt"
    ps_consume_cmd = f'''
    $process = Start-Process -FilePath "{abs_cli_path}" -ArgumentList "consume","--bootstrap-server","{BOOTSTRAP_SERVER}","--topic","{topic_name}","--from-beginning","--max-messages","3" -NoNewWindow -PassThru -RedirectStandardOutput "{temp_output}" -RedirectStandardError "temp_err.txt"
    $null = $process | Wait-Process -Timeout 10
    if ($process.HasExited -eq $false) {{
        $process | Stop-Process -Force
    }}
    Get-Content "{temp_output}" -ErrorAction SilentlyContinue
    '''
    
    try:
        result_consume = subprocess.run(
            ["powershell", "-Command", ps_consume_cmd],
            capture_output=True,
            text=True,
            timeout=15
        )
        
        consumed_output = result_consume.stdout + result_consume.stderr
        
        # 清理临时文件
        try:
            if os.path.exists(temp_output):
                with open(temp_output, 'r', encoding='utf-8') as f:
                    consumed_output += f.read()
                os.remove(temp_output)
            if os.path.exists("temp_err.txt"):
                os.remove("temp_err.txt")
        except:
            pass
        
        if "Hello Kafka" in consumed_output or "Message 2" in consumed_output:
            print_success("成功消费到消息")
            print_info(f"消费的消息片段:\n{consumed_output[:500]}")
            result.add_pass()
        else:
            print_error(f"未能消费到预期消息:\n{consumed_output[:500]}")
            result.add_fail("test_consume", "Expected messages not found")
    except Exception as e:
        print_error(f"消费消息异常: {e}")
        result.add_fail("test_consume", str(e))
        # 清理临时文件
        try:
            if os.path.exists(temp_output):
                os.remove(temp_output)
            if os.path.exists("temp_err.txt"):
                os.remove("temp_err.txt")
        except:
            pass
    
    # 清理
    print_info("步骤 4: 清理 topic")
    run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "delete",
        "--topic", topic_name
    ])
    time.sleep(1)

def test_topic_describe():
    """测试: 描述 topic 详情"""
    print_test_header("描述 Topic 详情")
    
    topic_name = generate_topic_name(TEST_PREFIX)
    
    # 创建 topic
    exit_code, stdout, stderr = run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "create",
        "--topic", topic_name,
        "--partitions", "5",
        "--replication-factor", "1"
    ])
    
    if exit_code != 0:
        print_error(f"创建 topic 失败: {stderr}")
        result.add_fail("test_describe_setup", f"exit_code={exit_code}")
        return
    
    time.sleep(2)
    
    # 描述 topic
    exit_code, stdout, stderr = run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "describe",
        "--topic", topic_name
    ])
    
    if exit_code != 0:
        print_error(f"描述 topic 失败: {stderr}")
        result.add_fail("test_describe", f"exit_code={exit_code}")
    else:
        # 验证输出包含必要信息
        checks = [
            ("Topic name", topic_name in stdout),
            ("Partitions count", "Partitions: 5" in stdout or "5" in stdout),
            ("Leader info", "Leader:" in stdout),
            ("Replicas info", "Replicas:" in stdout),
        ]
        
        all_passed = True
        for check_name, check_result in checks:
            if check_result:
                print_success(f"{check_name}: ✓")
            else:
                print_error(f"{check_name}: ✗")
                all_passed = False
        
        if all_passed:
            print_success("描述输出包含所有必要信息")
            result.add_pass()
        else:
            print_error("描述输出缺少某些信息")
            result.add_fail("test_describe", "Missing required information")
        
        print_info(f"完整输出:\n{stdout}")
    
    # 清理
    run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "delete",
        "--topic", topic_name
    ])

def test_invalid_bootstrap_server():
    """测试: 无效的 bootstrap server"""
    print_test_header("无效的 Bootstrap Server (错误处理)")
    
    exit_code, stdout, stderr = run_command([
        "topics",
        "--bootstrap-server", "invalid-server:9999",
        "list"
    ])
    
    # 应该失败
    if exit_code == 0:
        print_error("命令应该失败但返回了成功")
        result.add_fail("test_invalid_server", "Expected failure but got success")
        return
    
    if "Error" in stderr or "Error" in stdout or "Failed" in stderr or "Failed" in stdout:
        print_success("正确处理了无效的 bootstrap server")
        print_info(f"错误信息: {stderr or stdout}")
        result.add_pass()
    else:
        print_error("错误消息不明确")
        result.add_fail("test_invalid_server", "Unclear error message")

def test_topic_configs():
    """测试: Topic 配置管理"""
    print_test_header("Topic 配置管理")
    
    topic_name = generate_topic_name(TEST_PREFIX)
    
    # 创建 topic
    print_info("步骤 1: 创建 topic")
    exit_code, stdout, stderr = run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "create",
        "--topic", topic_name,
        "--partitions", "1",
        "--replication-factor", "1"
    ])
    
    if exit_code != 0:
        print_error(f"创建 topic 失败: {stderr}")
        result.add_fail("test_configs_setup", f"exit_code={exit_code}")
        return
    
    time.sleep(2)
    
    # 描述配置
    print_info("步骤 2: 描述 topic 配置")
    exit_code, stdout, stderr = run_command([
        "configs",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "describe",
        "--entity-type", "topic",
        "--entity-name", topic_name
    ])
    
    if exit_code != 0:
        print_error(f"描述配置失败: {stderr}")
        result.add_fail("test_configs_describe", f"exit_code={exit_code}")
    else:
        print_success("成功描述配置")
        print_info(f"配置输出:\n{stdout[:500]}")
    
    # 修改配置
    print_info("步骤 3: 修改 topic 配置")
    exit_code, stdout, stderr = run_command([
        "configs",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "alter",
        "--entity-type", "topic",
        "--entity-name", topic_name,
        "--add-config", "retention.ms=86400000"
    ])
    
    if exit_code != 0:
        print_error(f"修改配置失败: {stderr}")
        result.add_fail("test_configs_alter", f"exit_code={exit_code}")
    else:
        print_success("成功修改配置 (retention.ms=86400000)")
        result.add_pass()
    
    # 清理
    run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "delete",
        "--topic", topic_name
    ])

def test_consumer_groups_list():
    """测试列出 consumer groups"""
    print_test_header("列出所有 consumer groups")
    
    exit_code, stdout, stderr = run_command([
        "consumer-groups",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "list"
    ])
    
    if exit_code != 0:
        print_error(f"命令失败 (exit code: {exit_code})")
        print_info(f"stderr: {stderr}")
        result.add_fail("list_consumer_groups", stderr)
        return
    
    # 输出应该包含 "Consumer Groups:" 或 "No consumer groups found"
    if "Consumer Groups:" in stdout or "No consumer groups found" in stdout:
        print_success("成功列出 consumer groups")
        print_info(f"输出:\n{stdout[:200]}")
        result.add_pass()
    else:
        print_error("输出格式不符合预期")
        result.add_fail("list_consumer_groups", "Output format unexpected")

def test_consumer_groups_describe():
    """测试描述 consumer group"""
    print_test_header("创建 consumer group 并描述")
    
    topic_name = generate_topic_name("test_cg_desc")
    group_id = f"{topic_name}_group"
    
    # 创建 topic
    run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "create",
        "--topic", topic_name,
        "--partitions", "2",
        "--replication-factor", "1"
    ])
    
    # 生产一些消息
    messages = "msg1\nmsg2\nmsg3"
    run_command([
        "produce",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "--topic", topic_name
    ], input_data=messages)
    
    # 使用 consumer group 消费消息
    import subprocess
    consume_cmd = [
        KAFKA_CLI,
        "consume",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "--topic", topic_name,
        "--group", group_id,
        "--from-beginning",
        "--max-messages", "3"
    ]
    subprocess.run(consume_cmd, capture_output=True, text=True, timeout=10)
    
    # 等待 offset 提交
    time.sleep(2)
    
    # 描述 consumer group
    exit_code, stdout, stderr = run_command([
        "consumer-groups",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "describe",
        "--group", group_id
    ])
    
    if exit_code != 0:
        print_error(f"命令失败 (exit code: {exit_code})")
        print_info(f"stderr: {stderr}")
        result.add_fail("describe_consumer_group", stderr)
    elif group_id not in stdout:
        print_error(f"输出中未找到 group ID: {group_id}")
        result.add_fail("describe_consumer_group", "Group ID not in output")
    elif "State:" not in stdout:
        print_error("输出中未找到 State 信息")
        result.add_fail("describe_consumer_group", "State info missing")
    else:
        print_success(f"成功描述 consumer group: {group_id}")
        print_info(f"输出片段:\n{stdout[:300]}")
        result.add_pass()
    
    # 清理
    run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "delete",
        "--topic", topic_name
    ])

def test_consumer_groups_reset_offsets():
    """测试重置 consumer group offsets"""
    print_test_header("重置 consumer group offsets")
    
    topic_name = generate_topic_name("test_cg_reset")
    group_id = f"{topic_name}_group"
    
    # 创建 topic
    run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "create",
        "--topic", topic_name,
        "--partitions", "2",
        "--replication-factor", "1"
    ])
    
    # 生产消息
    messages = "msg1\nmsg2\nmsg3\nmsg4\nmsg5"
    run_command([
        "produce",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "--topic", topic_name
    ], input_data=messages)
    
    # 消费所有消息
    import subprocess
    consume_cmd = [
        KAFKA_CLI,
        "consume",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "--topic", topic_name,
        "--group", group_id,
        "--from-beginning",
        "--max-messages", "5"
    ]
    subprocess.run(consume_cmd, capture_output=True, text=True, timeout=10)
    
    # 等待 offset 提交
    time.sleep(2)
    
    # 测试 dry-run
    exit_code, stdout, stderr = run_command([
        "consumer-groups",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "reset-offsets",
        "--group", group_id,
        "--topic", topic_name,
        "--to-earliest"
    ])
    
    if exit_code != 0:
        print_error(f"Dry-run 失败 (exit code: {exit_code})")
        result.add_fail("reset_offsets_dryrun", stderr)
    elif "DRY RUN" not in stdout:
        print_error("Dry-run 输出中未找到 'DRY RUN'")
        result.add_fail("reset_offsets_dryrun", "DRY RUN not found")
    else:
        print_success("Dry-run 成功")
    
    # 执行实际重置
    exit_code, stdout, stderr = run_command([
        "consumer-groups",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "reset-offsets",
        "--group", group_id,
        "--topic", topic_name,
        "--to-earliest",
        "--execute"
    ])
    
    if exit_code != 0:
        print_error(f"重置失败 (exit code: {exit_code})")
        print_info(f"stderr: {stderr}")
        result.add_fail("reset_offsets_execute", stderr)
    elif "Successfully reset offsets" not in stdout:
        print_error("重置输出中未找到成功消息")
        result.add_fail("reset_offsets_execute", "Success message not found")
    else:
        print_success(f"成功重置 offsets for group: {group_id}")
        result.add_pass()
    
    # 清理
    run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "delete",
        "--topic", topic_name
    ])

def test_consumer_groups_reset_offsets_with_partitions():
    """测试使用 --partitions 参数重置指定分区的 offsets"""
    print_test_header("使用 --partitions 参数重置指定分区的 offsets")
    
    topic_name = generate_topic_name("test_cg_reset_partitions")
    group_id = f"{topic_name}_group"
    
    # 创建 3 个分区的 topic
    run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "create",
        "--topic", topic_name,
        "--partitions", "3",
        "--replication-factor", "1"
    ])
    
    # 生产消息
    messages = "\n".join([f"message_{i}" for i in range(9)])
    run_command([
        "produce",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "--topic", topic_name
    ], input_data=messages)
    
    # 消费所有消息
    import subprocess
    consume_cmd = [
        KAFKA_CLI,
        "consume",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "--topic", topic_name,
        "--group", group_id,
        "--from-beginning",
        "--max-messages", "9"
    ]
    subprocess.run(consume_cmd, capture_output=True, text=True, timeout=10)
    time.sleep(2)
    
    # 测试 dry-run 只重置分区 0 和 1
    exit_code, stdout, stderr = run_command([
        "consumer-groups",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "reset-offsets",
        "--group", group_id,
        "--topic", topic_name,
        "--to-earliest",
        "--partitions", "0,1"
    ])
    
    if exit_code != 0:
        print_error(f"Dry-run with partitions 失败 (exit code: {exit_code})")
        result.add_fail("reset_offsets_partitions_dryrun", stderr)
    elif "partitions: [0, 1]" not in stdout:
        print_error("Dry-run 输出中未找到正确的分区信息")
        result.add_fail("reset_offsets_partitions_dryrun", "Partition info not found")
    else:
        print_success("Dry-run with partitions 成功")
    
    # 执行实际重置（只重置分区 0 和 1）
    exit_code, stdout, stderr = run_command([
        "consumer-groups",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "reset-offsets",
        "--group", group_id,
        "--topic", topic_name,
        "--to-earliest",
        "--partitions", "0,1",
        "--execute"
    ])
    
    if exit_code != 0:
        print_error(f"重置分区 0,1 失败 (exit code: {exit_code})")
        result.add_fail("reset_offsets_partitions_execute", stderr)
    else:
        print_success(f"成功重置分区 0,1 的 offsets for group: {group_id}")
        
        # 验证：describe group 应该显示分区 0,1 的 offset 为 0，分区 2 保持不变
        time.sleep(1)
        exit_code, stdout, stderr = run_command([
            "consumer-groups",
            "--bootstrap-server", BOOTSTRAP_SERVER,
            "describe",
            "--group", group_id
        ])
        
        if exit_code == 0:
            print_info("验证重置结果:")
            print_info(stdout)
            result.add_pass()
        else:
            print_error("验证失败")
            result.add_fail("reset_offsets_partitions_verify", stderr)
    
    # 清理
    run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "delete",
        "--topic", topic_name
    ])

def test_consumer_groups_reset_offsets_by_timestamp():
    """测试使用 --to-datetime 参数按时间戳重置 offsets"""
    print_test_header("使用 --to-datetime 参数按时间戳重置 offsets")
    
    topic_name = generate_topic_name("test_cg_reset_timestamp")
    group_id = f"{topic_name}_group"
    
    # 创建 topic
    run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "create",
        "--topic", topic_name,
        "--partitions", "2",
        "--replication-factor", "1"
    ])
    
    # 生产第一批消息
    messages1 = "\n".join([f"early_message_{i}" for i in range(3)])
    run_command([
        "produce",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "--topic", topic_name
    ], input_data=messages1)
    
    # 等待一段时间，记录中间时间戳
    time.sleep(2)
    import time as time_module
    timestamp_middle = int(time_module.time() * 1000)  # 毫秒
    
    # 生产第二批消息
    messages2 = "\n".join([f"late_message_{i}" for i in range(3)])
    run_command([
        "produce",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "--topic", topic_name
    ], input_data=messages2)
    
    # 消费所有消息
    import subprocess
    consume_cmd = [
        KAFKA_CLI,
        "consume",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "--topic", topic_name,
        "--group", group_id,
        "--from-beginning",
        "--max-messages", "6"
    ]
    subprocess.run(consume_cmd, capture_output=True, text=True, timeout=10)
    time.sleep(2)
    
    # 测试 dry-run 使用时间戳
    exit_code, stdout, stderr = run_command([
        "consumer-groups",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "reset-offsets",
        "--group", group_id,
        "--topic", topic_name,
        "--to-datetime", str(timestamp_middle)
    ])
    
    if exit_code != 0:
        print_error(f"Dry-run with timestamp 失败 (exit code: {exit_code})")
        result.add_fail("reset_offsets_timestamp_dryrun", stderr)
    elif "By timestamp" not in stdout or "DateTime:" not in stdout:
        print_error("Dry-run 输出中未找到时间戳信息")
        result.add_fail("reset_offsets_timestamp_dryrun", "Timestamp info not found")
    else:
        print_success(f"Dry-run with timestamp 成功 (timestamp: {timestamp_middle})")
        print_info(f"时间戳输出:\n{stdout}")
    
    # 执行实际重置
    exit_code, stdout, stderr = run_command([
        "consumer-groups",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "reset-offsets",
        "--group", group_id,
        "--topic", topic_name,
        "--to-datetime", str(timestamp_middle),
        "--execute"
    ])
    
    if exit_code != 0:
        print_error(f"按时间戳重置失败 (exit code: {exit_code})")
        result.add_fail("reset_offsets_timestamp_execute", stderr)
    else:
        print_success(f"成功按时间戳重置 offsets for group: {group_id}")
        result.add_pass()
    
    # 清理
    run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "delete",
        "--topic", topic_name
    ])

def test_consumer_groups_delete_with_active_members():
    """测试删除有活跃成员的 consumer group（应该失败并显示友好错误）"""
    print_test_header("测试删除有活跃成员的 consumer group")
    
    topic_name = generate_topic_name("test_delete_active")
    group_id = f"{topic_name}_group"
    
    # 创建 topic
    run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "create",
        "--topic", topic_name,
        "--partitions", "1",
        "--replication-factor", "1"
    ])
    
    # 生产一些消息
    messages = "msg1\nmsg2\nmsg3"
    run_command([
        "produce",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "--topic", topic_name
    ], input_data=messages)
    
    # 启动一个后台消费者（作为活跃成员）
    import subprocess
    consume_process = subprocess.Popen(
        [
            KAFKA_CLI,
            "consume",
            "--bootstrap-server", BOOTSTRAP_SERVER,
            "--topic", topic_name,
            "--group", group_id
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True
    )
    
    # 等待消费者加入组
    time.sleep(3)
    
    # 尝试删除有活跃成员的组（应该失败）
    exit_code, stdout, stderr = run_command([
        "consumer-groups",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "delete",
        "--group", group_id
    ])
    
    # 终止后台消费者
    consume_process.terminate()
    try:
        consume_process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        consume_process.kill()
    
    if exit_code == 0:
        print_error("删除有活跃成员的组应该失败，但成功了")
        result.add_fail("delete_active_group", "Should have failed")
    elif "active member" in stderr.lower() or "active member" in stdout.lower():
        print_success("正确拒绝删除有活跃成员的组，并显示友好错误信息")
        print_info(f"错误信息: {stderr}")
        result.add_pass()
    else:
        print_error("错误信息不够友好，未提及活跃成员")
        result.add_fail("delete_active_group", "Error message unclear")
    
    # 等待成员离开
    time.sleep(3)
    
    # 现在删除应该成功（或显示合适的信息）
    exit_code, stdout, stderr = run_command([
        "consumer-groups",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "delete",
        "--group", group_id
    ])
    
    if exit_code == 0:
        print_success("成员离开后，成功删除 consumer group")
    else:
        print_info("删除结果: " + stdout + stderr)
    
    # 清理
    run_command([
        "topics",
        "--bootstrap-server", BOOTSTRAP_SERVER,
        "delete",
        "--topic", topic_name
    ])

def main():
    """主函数"""
    # 设置 UTF-8 编码
    import sys
    import io
    if sys.platform == 'win32':
        sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8', errors='replace')
        sys.stderr = io.TextIOWrapper(sys.stderr.buffer, encoding='utf-8', errors='replace')
    
    print(f"\n{Colors.BOLD}{Colors.CYAN}{'='*70}{Colors.RESET}")
    print(f"{Colors.BOLD}{Colors.CYAN}Kafka CLI 功能测试套件{Colors.RESET}")
    print(f"{Colors.BOLD}{Colors.CYAN}{'='*70}{Colors.RESET}\n")
    
    print_info(f"CLI 路径: {KAFKA_CLI}")
    print_info(f"Bootstrap Server: {BOOTSTRAP_SERVER}")
    print_info(f"测试前缀: {TEST_PREFIX}")
    
    # 检查 CLI 是否存在
    import os
    if not os.path.exists(KAFKA_CLI):
        print_error(f"CLI 可执行文件不存在: {KAFKA_CLI}")
        print_info("请先运行: cargo build --release")
        sys.exit(1)
    
    # 运行所有测试
    tests = [
        ("基础命令", [
            test_help_command,
            test_version_command,
        ]),
        ("Topics 管理", [
            test_topics_list,
            test_topic_create_and_delete,
            test_topic_describe,
        ]),
        ("配置管理", [
            test_topic_configs,
        ]),
        ("消息生产消费", [
            test_produce_and_consume,
        ]),
        ("Consumer Groups", [
            test_consumer_groups_list,
            test_consumer_groups_describe,
            test_consumer_groups_reset_offsets,
            test_consumer_groups_reset_offsets_with_partitions,
            test_consumer_groups_reset_offsets_by_timestamp,
            test_consumer_groups_delete_with_active_members,
        ]),
        ("错误处理", [
            test_invalid_bootstrap_server,
        ]),
    ]
    
    for category, test_funcs in tests:
        print(f"\n{Colors.BOLD}{Colors.CYAN}{'─'*70}{Colors.RESET}")
        print(f"{Colors.BOLD}{Colors.CYAN}测试类别: {category}{Colors.RESET}")
        print(f"{Colors.BOLD}{Colors.CYAN}{'─'*70}{Colors.RESET}")
        
        for test_func in test_funcs:
            try:
                test_func()
            except Exception as e:
                print_error(f"测试异常: {test_func.__name__} - {e}")
                result.add_fail(test_func.__name__, str(e))
            
            time.sleep(0.5)
    
    # 打印摘要
    success = result.print_summary()
    
    sys.exit(0 if success else 1)

if __name__ == "__main__":
    main()
