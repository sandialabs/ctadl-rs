import os
import sys
import subprocess
import shutil
import glob

def run_cmd(cmd, **kwargs):
    print(f"[*] Running: {' '.join(cmd)}")
    subprocess.run(cmd, check=True, **kwargs)

def check_dependencies():
    print("[*] Checking dependencies...")
    missing = False
    if not shutil.which("binwalk"):
        print("[!] Error: 'binwalk' is not installed. Please install it (e.g., sudo apt install binwalk, or via brew).")
        missing = True
        
    try:
        import gdown
    except ImportError:
        print("[*] 'gdown' python module not found. Installing it now...")
        run_cmd([sys.executable, "-m", "pip", "install", "gdown"])
        
    if missing:
        sys.exit(1)

def download_dataset():
    import gdown
    file_id = "1-VOf-tEpu4LIgyDyZr7bBZCDK-K2DHaj"
    url = f"https://drive.google.com/uc?id={file_id}"
    
    download_dir = "benchmarks/karonte_download"
    os.makedirs(download_dir, exist_ok=True)
    
    # Check if we already downloaded something
    existing_files = [f for f in os.listdir(download_dir) if not f.startswith('.')]
    if existing_files:
        archive_path = os.path.join(download_dir, existing_files[0])
        print(f"[*] Found existing archive: {archive_path}. Skipping download.")
        return archive_path
        
    print("[*] Downloading Karonte dataset from Google Drive...")
    
    # Change dir so gdown saves it there with original filename
    original_cwd = os.getcwd()
    os.chdir(download_dir)
    try:
        archive_path = gdown.download(url, quiet=False, fuzzy=True)
    finally:
        os.chdir(original_cwd)
        
    if archive_path is None:
        print("[!] Failed to download dataset.")
        sys.exit(1)
        
    return os.path.join(download_dir, os.path.basename(archive_path))

def extract_archive(archive_path, extract_dir):
    print(f"[*] Extracting archive {archive_path} to {extract_dir}...")
    os.makedirs(extract_dir, exist_ok=True)
    shutil.unpack_archive(archive_path, extract_dir)

def run_binwalk(firmware_dir):
    print("[*] Running binwalk on firmware images...")
    
    # Find all files that are likely firmware (ignore directories)
    firmware_files = []
    for root, dirs, files in os.walk(firmware_dir):
        for file in files:
            file_path = os.path.join(root, file)
            if not file.startswith('.') and os.path.isfile(file_path):
                # Avoid re-extracting things inside .extracted directories
                if ".extracted" not in root:
                    firmware_files.append(file_path)
                
    for fw in firmware_files:
        # Check if already extracted
        extract_target = fw + ".extracted"
        if os.path.exists(extract_target):
            print(f"[*] Already extracted: {fw}")
            continue
            
        print(f"[*] Extracting firmware: {fw}")
        # Use subprocess.run without check=True to handle partial failures (like symlink errors)
        # and continue with the rest of the extraction.
        cmd = ["binwalk", "-Me", fw, "-C", os.path.dirname(fw)]
        print(f"[*] Running: {' '.join(cmd)}")
        res = subprocess.run(cmd, capture_output=True, text=True)
        
        if res.returncode != 0:
            print(f"[!] Warning: binwalk reported an issue (code {res.returncode}) on {fw}.")
            if "Failed to create symlink" in res.stderr:
                print("    (Likely a symlink collision or filesystem issue; continuing anyway...)")
            else:
                print(f"    Error (first 200 chars): {res.stderr[:200]}...")
        else:
            print(f"[*] Successfully extracted {fw}")

def gather_elfs(firmware_dir, output_dir):
    print(f"[*] Gathering ELF binaries into {output_dir}...")
    os.makedirs(output_dir, exist_ok=True)
    
    elf_count = 0
    for root, dirs, files in os.walk(firmware_dir):
        # Only look inside extracted directories
        if ".extracted" not in root:
            continue
            
        for file in files:
            file_path = os.path.join(root, file)
            # Basic check: skip known non-binaries or symlinks
            if os.path.islink(file_path) or not os.path.isfile(file_path):
                continue
                
            # Use 'file' command to check if it's an ELF
            try:
                result = subprocess.run(["file", file_path], capture_output=True, text=True)
                if "ELF" in result.stdout and "executable" in result.stdout:
                    # Found an ELF. Copy it to the output directory.
                    # We prefix with the firmware name to avoid collisions
                    fw_name = root.split(".extracted")[0].split("/")[-1]
                    new_name = f"{fw_name}_{file}"
                    dest_path = os.path.join(output_dir, new_name)
                    
                    if not os.path.exists(dest_path):
                        shutil.copy2(file_path, dest_path)
                    elf_count += 1
            except Exception:
                pass
                
    print(f"[*] Successfully gathered {elf_count} ELF binaries into {output_dir}")

def main():
    check_dependencies()
    
    archive_path = download_dataset()
    
    firmware_dir = "benchmarks/karonte_firmware"
    if not os.path.exists(firmware_dir):
        extract_archive(archive_path, firmware_dir)
    else:
        print(f"[*] Archive already extracted to {firmware_dir}")
        
    run_binwalk(firmware_dir)
    
    elf_dir = "benchmarks/karonte_elfs"
    gather_elfs(firmware_dir, elf_dir)
    
    print("\n" + "="*50)
    print("Setup Complete!")
    print(f"You can now run the evaluation script on the extracted binaries:")
    print(f"python3 scripts/evaluate_benchmarks.py {elf_dir} --model benchmarks/firmware_model.json")
    print("="*50)

if __name__ == "__main__":
    main()
