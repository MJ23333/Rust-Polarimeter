#!/usr/bin/env python3
# -*- coding: utf-8 -*-

import os
import shutil
import subprocess
import sys
import re
from pathlib import Path
from collections import deque
import argparse
import stat

# 尝试导入 graphviz，如果失败也没关系，只有在用户请求时才需要它
try:
    import graphviz
except ImportError:
    graphviz = None

# --- 全局变量和常量 ---

HOMEBREW_PREFIX = None

# --- 辅助函数 ---

def get_brew_prefix():
    """获取 Homebrew 的安装路径。"""
    global HOMEBREW_PREFIX
    if HOMEBREW_PREFIX:
        return HOMEBREW_PREFIX
    
    # 标准路径
    if Path("/opt/homebrew/bin/brew").exists():
        HOMEBREW_PREFIX = "/opt/homebrew"
    elif Path("/usr/local/bin/brew").exists():
        HOMEBREW_PREFIX = "/usr/local"
    else:
        # 后备方案：使用命令
        try:
            result = subprocess.run(["brew", "--prefix"], capture_output=True, text=True, check=True)
            HOMEBREW_PREFIX = result.stdout.strip()
        except (subprocess.CalledProcessError, FileNotFoundError):
            print("错误：无法找到 Homebrew。请确保它已安装并在您的 PATH 中。", file=sys.stderr)
            sys.exit(1)
    
    print(f"[*] 检测到 Homebrew 安装路径: {HOMEBREW_PREFIX}")
    return HOMEBREW_PREFIX

def run_command(command, check=True):
    """执行一个 shell 命令并返回其输出。"""
    # print(f"  执行: {' '.join(command)}") # 用于调试，可能会输出过多信息
    try:
        result = subprocess.run(command, capture_output=True, text=True, check=check)
        return result.stdout.strip()
    except subprocess.CalledProcessError as e:
        print(f"  错误: 命令 '{' '.join(command)}' 执行失败。", file=sys.stderr)
        print(f"  退出码: {e.returncode}", file=sys.stderr)
        print(f"  标准输出:\n{e.stdout}", file=sys.stderr)
        print(f"  标准错误:\n{e.stderr}", file=sys.stderr)
        if check:
            sys.exit(1)
        return None

def get_mach_o_dependencies(file_path):
    """使用 otool -L 获取一个二进制文件的所有动态库依赖。"""
    output = run_command(["otool", "-L", str(file_path)])
    dep_pattern = re.compile(r"^\s*(.+?)\s*\(compatibility version")
    dependencies = []
    # 跳过第一行，因为那是文件自身的ID
    for line in output.splitlines()[1:]:
        match = dep_pattern.match(line.strip())
        if match:
            dependencies.append(match.group(1))
    return dependencies

def get_mach_o_rpaths(file_path):
    """使用 otool -l 获取二进制文件的 rpath 列表。"""
    output = run_command(["otool", "-l", str(file_path)])
    rpath_pattern = re.compile(r"^\s+path\s+(.+?)\s*\(offset \d+\)")
    rpaths = []
    for line in output.splitlines():
        match = rpath_pattern.match(line)
        if match:
            rpaths.append(match.group(1))
    return rpaths

def resolve_dependency_path(dep_path, binary_original_path):
    """
    根据二进制文件的原始位置，正确解析其依赖项路径，特别是处理 @rpath。
    """
    if not dep_path.startswith('@'):
        return Path(dep_path)

    binary_original_dir = binary_original_path.parent
    
    if dep_path.startswith("@loader_path/"):
        resolved = (binary_original_dir / dep_path.replace("@loader_path/", "")).resolve()
        if resolved.exists():
            return resolved

    if dep_path.startswith("@rpath/"):
        sub_path = dep_path.replace("@rpath/", "")
        original_rpaths = get_mach_o_rpaths(binary_original_path)
        for rpath in original_rpaths:
            # 关键：rpath 内部的 @loader_path 需要相对于原始二进制文件的位置进行解析
            if rpath.startswith("@loader_path"):
                base_path = rpath.replace("@loader_path", str(binary_original_dir))
            else:
                base_path = rpath
            
            potential_path = (Path(base_path) / sub_path).resolve()
            if potential_path.exists():
                return potential_path
    
    # 针对 rpath 配置不正确的后备方案：直接在 Homebrew 的主 lib 目录中搜索
    if dep_path.startswith("@rpath/"):
        dylib_name = Path(dep_path).name
        potential_path = Path(get_brew_prefix()) / "lib" / dylib_name
        if potential_path.exists():
            print(f"  警告: '{dep_path}' 无法通过 '{binary_original_path.name}' 的 rpath 解析。")
            print(f"    -> 在 Homebrew lib 目录中找到后备: {potential_path}")
            return potential_path

    return None

# --- 核心逻辑 ---

def build_dependency_graph(initial_files, brew_prefix, graph=None):
    """
    递归扫描文件，建立所有需要的 Homebrew 依赖项的图谱。
    如果提供了 graph 对象，则同时填充该图。
    """
    print("\n--- 第1阶段: 发现依赖项 ---")
    
    dependency_map = {}
    queue = deque(initial_files)
    scanned_paths = set(initial_files)
    graph_nodes = set() # 跟踪已添加到图中的节点名

    while queue:
        current_path = queue.popleft()
        
        if graph:
            node_name = current_path.name
            if node_name not in graph_nodes:
                if current_path in initial_files:
                    graph.node(node_name, shape='box', style='filled', color='lightblue')
                elif str(current_path).startswith(brew_prefix):
                    graph.node(node_name, style='filled', color='orange')
                graph_nodes.add(node_name)
        
        print(f"[*] 扫描: {current_path}")

        if not current_path.exists():
            print(f"  警告: 文件不存在，跳过: {current_path}")
            continue

        direct_deps = get_mach_o_dependencies(current_path)
        dependency_map[current_path] = {'original_deps': {}}

        for dep_str in direct_deps:
            resolved_dep_path = resolve_dependency_path(dep_str, current_path)
            
            if graph:
                source_node_name = current_path.name
                if resolved_dep_path:
                    dest_node_name = resolved_dep_path.name
                    if dest_node_name not in graph_nodes:
                        if str(resolved_dep_path).startswith(brew_prefix):
                            graph.node(dest_node_name, style='filled', color='orange')
                        else:
                            graph.node(dest_node_name, style='filled', color='lightgrey')
                        graph_nodes.add(dest_node_name)
                    graph.edge(source_node_name, dest_node_name)
                else:
                    unresolved_name = f"Unresolved:\n{Path(dep_str).name}"
                    if unresolved_name not in graph_nodes:
                        graph.node(unresolved_name, shape='diamond', style='filled', color='red')
                        graph_nodes.add(unresolved_name)
                    graph.edge(source_node_name, unresolved_name)

            if not resolved_dep_path:
                continue
            
            dependency_map[current_path]['original_deps'][dep_str] = resolved_dep_path

            if str(resolved_dep_path).startswith(brew_prefix) and resolved_dep_path not in scanned_paths:
                scanned_paths.add(resolved_dep_path)
                queue.append(resolved_dep_path)
                
    print("--- 发现阶段完成 ---")
    return dependency_map


def package_dependencies(app_path_str, graph_output_file=None):
    """处理 .app 文件包的主函数。"""
    app_path = Path(app_path_str).resolve()
    if not app_path.is_dir() or not app_path.suffix == ".app":
        print(f"错误：'{app_path}' 不是一个有效的 .app 文件包。", file=sys.stderr)
        sys.exit(1)
        
    print(f"[*] 开始处理应用: {app_path.name}")

    contents_path = app_path / "Contents"
    macos_path = contents_path / "MacOS"
    frameworks_path = contents_path / "Frameworks"
    frameworks_path.mkdir(exist_ok=True)
    print(f"[*] 确保 Frameworks 目录存在: {frameworks_path}")

    executables = [f for f in macos_path.iterdir() if f.is_file() and os.access(f, os.X_OK)]
    if not executables:
        print(f"错误：在 {macos_path} 中找不到可执行文件。", file=sys.stderr)
        sys.exit(1)

    graph = None
    if graph_output_file:
        if not graphviz:
            print("错误: 需要 'graphviz' Python 库来生成依赖图。", file=sys.stderr)
            print("请运行: pip install graphviz", file=sys.stderr)
            print("并且确保 Graphviz 系统软件已安装 (brew install graphviz)", file=sys.stderr)
            sys.exit(1)
        graph = graphviz.Digraph('DependencyGraph', comment=f'Dependencies for {app_path.name}', graph_attr={'rankdir': 'LR'})

    brew_prefix = get_brew_prefix()
    
    dependency_graph = build_dependency_graph(executables, brew_prefix, graph=graph)
    
    if graph and graph_output_file:
        try:
            output_path = Path(graph_output_file)
            # render 函数会自动添加后缀，所以我们提供不带后缀的文件名
            graph.render(output_path.with_suffix(''), view=False, cleanup=True)
            print(f"\n[+] 依赖图已保存到: {output_path}")
        except Exception as e:
            print(f"\n错误: 无法生成依赖图文件。", file=sys.stderr)
            print(e, file=sys.stderr)
    
    # --- 第2阶段: 复制、修补和重链接 ---
    print("\n--- 第2阶段: 复制和修补依赖项 ---")
    
    homebrew_dylibs_to_copy = set()
    for data in dependency_graph.values():
        for resolved_dep_path in data['original_deps'].values():
            if str(resolved_dep_path).startswith(brew_prefix):
                homebrew_dylibs_to_copy.add(resolved_dep_path)

    for dylib_path in sorted(list(homebrew_dylibs_to_copy)):
        dylib_name = dylib_path.name
        dest_path = frameworks_path / dylib_name
        new_id = f"@rpath/{dylib_name}"

        print(f"[*] 处理: {dylib_name}")
        if not dest_path.exists():
            print(f"  -> 复制: {dylib_path} -> {dest_path}")
            shutil.copy2(dylib_path, dest_path)
        else:
            print("  -> 已存在于 Frameworks 目录, 跳过复制。")
        
        dest_path.chmod(dest_path.stat().st_mode | stat.S_IWUSR)

        print(f"  -> 修改 ID 为: {new_id}")
        run_command(["install_name_tool", "-id", new_id, str(dest_path)])

        dylib_deps_data = dependency_graph.get(dylib_path)
        if dylib_deps_data:
            for original_dep_str, resolved_dep_path in dylib_deps_data['original_deps'].items():
                if str(resolved_dep_path).startswith(brew_prefix):
                    new_dep_path = f"@rpath/{resolved_dep_path.name}"
                    print(f"  -> 修改内部链接: {original_dep_str} -> {new_dep_path}")
                    run_command(["install_name_tool", "-change", original_dep_str, new_dep_path, str(dest_path)])

    # --- 第3阶段: 修补主可执行文件 ---
    print("\n--- 第3阶段: 修补主可执行文件 ---")
    for executable in executables:
        print(f"[*] 修补: {executable.name}")
        executable.chmod(executable.stat().st_mode | stat.S_IWUSR)

        executable_deps_data = dependency_graph.get(executable)
        if executable_deps_data:
            for original_dep_str, resolved_dep_path in executable_deps_data['original_deps'].items():
                if str(resolved_dep_path).startswith(brew_prefix):
                    new_dep_path = f"@rpath/{resolved_dep_path.name}"
                    print(f"  -> 修改链接: {original_dep_str} -> {new_dep_path}")
                    run_command(["install_name_tool", "-change", original_dep_str, new_dep_path, str(executable)])
        
        print(f"  -> 添加 rpath: @executable_path/../Frameworks")
        try:
            run_command(["install_name_tool", "-delete_rpath", "@executable_path/../Frameworks", str(executable)], check=False)
        except Exception:
            pass
        run_command(["install_name_tool", "-add_rpath", "@executable_path/../Frameworks", str(executable)])

    print("\n[+] 所有依赖处理完毕！")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="扫描一个 macOS .app 文件包，将 Homebrew 依赖项捆绑到其 Frameworks 目录中，并修正链接。",
        formatter_class=argparse.RawTextHelpFormatter
    )
    parser.add_argument(
        "app_bundle_path",
        help="要处理的 .app 文件包的路径。\n例如: /path/to/YourApp.app"
    )
    parser.add_argument(
        "--graph",
        metavar="FILENAME",
        help="生成依赖关系图并保存到指定文件 (例如: deps.pdf, deps.png)"
    )

    args = parser.parse_args()
    package_dependencies(args.app_bundle_path, graph_output_file=args.graph)

