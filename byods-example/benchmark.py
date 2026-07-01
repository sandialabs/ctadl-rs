import os
import subprocess
import statistics

input_filenames = [
    'data/smallA.facts',
    'data/smallB.facts',
    'data/smallC.facts',
    'data/mediumA.facts',
    'data/mediumB.facts',
    'data/mediumC.facts',
    'data/largeA.facts',
    'data/largeB.facts',
    'data/largeC.facts'
]

shell_command_template = 'cargo run {} lmdb'
NUM_TRIALS = 5

def extract_statistics(filename):
    command = shell_command_template.format(filename)
    wall_times = []
    max_rss_sizes = []
    num_paths = None

    for i in range(NUM_TRIALS):
        print(f"\tRunning trial {i}...")
        try:
            result = subprocess.run(command, shell=True, capture_output=True, text=True)
            if result.returncode == 0:
                output_lines = result.stdout.splitlines()
                for line in output_lines:
                    if 'Wall time (secs):' in line:
                        wall_time = float(line.split(':')[1].strip())
                        wall_times.append(wall_time)
                    elif 'Max resident set size:' in line:
                        max_rss_size = int(line.split(':')[1].strip())
                        max_rss_sizes.append(max_rss_size)
                    elif 'num paths:' in line:
                        current_num_paths = int(line.split(':')[1].strip())
                        if num_paths is None:
                            num_paths = current_num_paths
                        elif num_paths != current_num_paths:
                            print(f"Inconsistent num paths for {filename}: {num_paths} vs {current_num_paths}")
            else:
                print(f"Error running command on {filename}: {result.stderr}")
        except Exception as e:
            print(f"An error occurred: {e}")

    return wall_times, max_rss_sizes, num_paths

for filename in input_filenames:
    print(f"Running on file {filename}")
    wall_times, max_rss_sizes, num_paths = extract_statistics(filename)
    
    if wall_times and max_rss_sizes:
        median_wall_time = statistics.median(wall_times)
        median_max_rss_size = statistics.median(max_rss_sizes)
        
        print(f"Statistics for {filename}:")
        print(f"\tMedian Wall time (secs): {median_wall_time}")
        print(f"\tMedian Max resident set size: {median_max_rss_size}")
        print(f"\tNum paths: {num_paths}")
        print()
    else:
        print(f"No valid statistics found for {filename}.")

summary_results = []

for filename in input_filenames:
    print(f"Running on file {filename}")
    wall_times, max_rss_sizes, num_paths = extract_statistics(filename)
    
    if wall_times and max_rss_sizes:
        median_wall_time = statistics.median(wall_times)
        median_max_rss_size = statistics.median(max_rss_sizes)

        print(f"Median Wall time (secs): {median_wall_time}")
        print(f"Median Max resident set size: {median_max_rss_size}")
        print(f"Num paths: {num_paths}")
        print()
        
        base_filename = os.path.splitext(os.path.basename(filename))[0]
        summary_results.append((base_filename, median_wall_time, median_max_rss_size))
    else:
        print(f"No valid statistics found for {filename}.")


print("\n| File name | Median wall time (secs) | Median max resident set size |")
print("|-----------|-------------------------|------------------------------|")
for result in summary_results:
    print(f"| {result[0]} | {result[1]:.3f} | {result[2]} |")