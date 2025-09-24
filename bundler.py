#!/usr/bin/env python3
"""
macOS .app Bundle Dependency Resolver

This script analyzes a .app bundle, finds all Homebrew dependencies,
copies them to the Frameworks directory, updates linking paths,
and re-signs the bundle.
"""

import os
import sys
import subprocess
import shutil
import re
from pathlib import Path
from typing import Set, Dict, List, Optional


class MacAppBundler:
    def __init__(self, app_path: str):
        self.app_path = Path(app_path).resolve()
        if not self.app_path.exists() or not str(self.app_path).endswith('.app'):
            raise ValueError(f"Invalid .app path: {app_path}")

        self.frameworks_dir = self.app_path / "Contents" / "Frameworks"
        self.executables_dir = self.app_path / "Contents" / "MacOS"

        # Common Homebrew paths
        self.homebrew_paths = [
            "/opt/homebrew",  # Apple Silicon
            "/usr/local",     # Intel
            "/home/linuxbrew/.linuxbrew"  # Linux (just in case)
        ]

        self.processed_dylibs: Set[str] = set()
        self.rpath_cache: Dict[str, List[str]] = {}

    def run_command(self, cmd: List[str], check: bool = True) -> subprocess.CompletedProcess:
        """Run a shell command and return the result."""
        try:
            result = subprocess.run(cmd, capture_output=True, text=True, check=check)
            return result
        except subprocess.CalledProcessError as e:
            print(f"Command failed: {' '.join(cmd)}")
            print(f"Error: {e.stderr}")
            if check:
                raise
            return e

    def get_dylib_dependencies(self, binary_path: str) -> List[str]:
        """Get all dylib dependencies of a binary using otool."""
        result = self.run_command(['otool', '-L', binary_path])
        dependencies = []

        for line in result.stdout.splitlines()[1:]:  # Skip header
            line = line.strip()
            if line and not line.startswith('Archive'):
                # Extract the path (first part before compatibility version info)
                path = line.split('(')[0].strip()
                if path and path != binary_path:
                    dependencies.append(path)

        return dependencies

    def get_rpaths(self, binary_path: str) -> List[str]:
        """Get all rpath entries from a binary."""
        if binary_path in self.rpath_cache:
            return self.rpath_cache[binary_path]

        result = self.run_command(['otool', '-l', binary_path])
        rpaths = []

        lines = result.stdout.splitlines()
        i = 0
        while i < len(lines):
            line = lines[i].strip()
            if line.startswith('cmd LC_RPATH'):
                # Look for the path in the next few lines
                for j in range(i + 1, min(i + 10, len(lines))):
                    if 'path' in lines[j]:
                        path_line = lines[j].strip()
                        # Extract path from "path /some/path (offset X)"
                        match = re.search(r'path\s+(.+?)\s+\(offset', path_line)
                        if match:
                            rpaths.append(match.group(1))
                        break
            i += 1

        self.rpath_cache[binary_path] = rpaths
        return rpaths

    def resolve_rpath(self, dependency_path: str, binary_path: str) -> Optional[str]:
        """Resolve @rpath, @loader_path, @executable_path in dependency paths."""
        if not dependency_path.startswith('@'):
            return dependency_path

        if dependency_path.startswith('@rpath/'):
            rpaths = self.get_rpaths(binary_path)
            for rpath in rpaths:
                resolved_rpath = rpath.replace('@loader_path', str(Path(binary_path).parent))
                resolved_rpath = resolved_rpath.replace('@executable_path', str(self.executables_dir))
                full_path = os.path.join(resolved_rpath, dependency_path[7:])  # Remove '@rpath/'
                if os.path.exists(full_path):
                    return full_path

        elif dependency_path.startswith('@loader_path/'):
            base_path = str(Path(binary_path).parent)
            return os.path.join(base_path, dependency_path[13:])  # Remove '@loader_path/'

        elif dependency_path.startswith('@executable_path/'):
            return os.path.join(str(self.executables_dir), dependency_path[17:])  # Remove '@executable_path/'

        return None

    def is_homebrew_path(self, path: str) -> bool:
        """Check if a path is from Homebrew."""
        for homebrew_path in self.homebrew_paths:
            if path.startswith(homebrew_path):
                return True
        return False

    def find_all_executables(self) -> List[str]:
        """Find all executable files in the .app bundle."""
        executables = []

        # Main executables
        macos_dir = self.app_path / "Contents" / "MacOS"
        if macos_dir.exists():
            for item in macos_dir.iterdir():
                if item.is_file() and os.access(str(item), os.X_OK):
                    executables.append(str(item))

        # Frameworks
        if self.frameworks_dir.exists():
            for item in self.frameworks_dir.rglob('*'):
                if item.is_file() and (item.suffix in ['.dylib', ''] or 'framework' in str(item)):
                    try:
                        # Check if it's a Mach-O binary
                        result = self.run_command(['file', str(item)], check=False)
                        if 'Mach-O' in result.stdout:
                            executables.append(str(item))
                    except:
                        pass

        return executables

    def collect_homebrew_dependencies(self) -> Dict[str, str]:
        """Collect all Homebrew dependencies recursively."""
        homebrew_deps = {}
        to_process = self.find_all_executables()
        processed = set()

        while to_process:
            current = to_process.pop(0)
            if current in processed:
                continue

            processed.add(current)
            print(f"Analyzing: {current}")

            try:
                dependencies = self.get_dylib_dependencies(current)

                for dep in dependencies:
                    resolved_path = self.resolve_rpath(dep, current)
                    if resolved_path and os.path.exists(resolved_path):
                        if self.is_homebrew_path(resolved_path):
                            if resolved_path not in homebrew_deps:
                                homebrew_deps[resolved_path] = dep
                                to_process.append(resolved_path)
                                print(f"  Found Homebrew dependency: {resolved_path}")
                    elif self.is_homebrew_path(dep):
                        if os.path.exists(dep):
                            homebrew_deps[dep] = dep
                            to_process.append(dep)
                            print(f"  Found Homebrew dependency: {dep}")

            except Exception as e:
                print(f"  Error analyzing {current}: {e}")

        return homebrew_deps

    def copy_dependencies_to_frameworks(self, homebrew_deps: Dict[str, str]) -> None:
        """Copy Homebrew dependencies to the Frameworks directory."""
        self.frameworks_dir.mkdir(parents=True, exist_ok=True)

        for src_path, _ in homebrew_deps.items():
            dst_name = os.path.basename(src_path)
            dst_path = self.frameworks_dir / dst_name

            if not dst_path.exists():
                print(f"Copying {src_path} -> {dst_path}")
                shutil.copy2(src_path, dst_path)
                # Make it writable so we can modify it
                os.chmod(dst_path, 0o755)

    def update_linking(self, homebrew_deps: Dict[str, str]) -> None:
        """Update all linking paths using install_name_tool."""
        all_executables = self.find_all_executables()

        # Create mapping from old paths to new framework paths
        path_mapping = {}
        for src_path, original_ref in homebrew_deps.items():
            dst_name = os.path.basename(src_path)
            new_path = f"@loader_path/../Frameworks/{dst_name}"
            path_mapping[src_path] = new_path
            path_mapping[original_ref] = new_path

        for executable in all_executables:
            print(f"Updating links in: {executable}")

            try:
                dependencies = self.get_dylib_dependencies(executable)

                for dep in dependencies:
                    # Check direct matches
                    new_path = path_mapping.get(dep)

                    # Check resolved paths
                    if not new_path:
                        resolved = self.resolve_rpath(dep, executable)
                        if resolved:
                            new_path = path_mapping.get(resolved)

                    if new_path:
                        print(f"  Changing {dep} -> {new_path}")
                        cmd = ['install_name_tool', '-change', dep, new_path, executable]
                        self.run_command(cmd)

                # Update install name for dylibs
                if executable.endswith('.dylib') or '/Frameworks/' in executable:
                    basename = os.path.basename(executable)
                    new_install_name = f"@loader_path/{basename}"
                    cmd = ['install_name_tool', '-id', new_install_name, executable]
                    self.run_command(cmd, check=False)  # May fail for some files

            except Exception as e:
                print(f"  Error updating {executable}: {e}")

    def codesign_bundle(self, identity: Optional[str] = None) -> None:
        """Re-sign the entire bundle."""
        if identity is None:
            identity = "-"  # Ad-hoc signing

        print(f"Code signing with identity: {identity}")

        # Sign frameworks first
        if self.frameworks_dir.exists():
            for item in self.frameworks_dir.rglob('*'):
                if item.is_file() and (item.suffix == '.dylib' or 'framework' in str(item)):
                    try:
                        cmd = ['codesign', '--force', '--sign', identity, str(item)]
                        self.run_command(cmd)
                    except:
                        pass

        # Sign the main bundle
        cmd = ['codesign', '--force', '--sign', identity, '--deep', str(self.app_path)]
        self.run_command(cmd)

        print("Code signing completed")

    def process(self, codesign_identity: Optional[str] = None) -> None:
        """Main processing function."""
        print(f"Processing {self.app_path}")

        # Step 1: Collect Homebrew dependencies
        print("\n1. Collecting Homebrew dependencies...")
        homebrew_deps = self.collect_homebrew_dependencies()

        if not homebrew_deps:
            print("No Homebrew dependencies found!")
            return

        print(f"Found {len(homebrew_deps)} Homebrew dependencies")

        # Step 2: Copy dependencies to Frameworks
        print("\n2. Copying dependencies to Frameworks directory...")
        self.copy_dependencies_to_frameworks(homebrew_deps)

        # Step 3: Update linking
        print("\n3. Updating linking paths...")
        self.update_linking(homebrew_deps)

        # Step 4: Code signing
        print("\n4. Code signing...")
        self.codesign_bundle(codesign_identity)

        print("\nBundle processing completed successfully!")


def main():
    if len(sys.argv) < 2:
        print("Usage: python3 mac_app_bundler.py <app_path> [codesign_identity]")
        print("Example: python3 mac_app_bundler.py MyApp.app")
        print("Example: python3 mac_app_bundler.py MyApp.app 'Developer ID Application: Your Name'")
        sys.exit(1)

    app_path = sys.argv[1]
    codesign_identity = sys.argv[2] if len(sys.argv) > 2 else None

    try:
        bundler = MacAppBundler(app_path)
        bundler.process(codesign_identity)
    except Exception as e:
        print(f"Error: {e}")
        sys.exit(1)


if __name__ == "__main__":
    main()