#!/usr/bin/python3

# A script that ingests data pulled from Cobalt metrics to analyze the relationship between
# poll round trip times and the probablity distribution of the offset of the actual time
# versus the reported time.
# https://fuchsia.googlesource.com/cobalt-registry/+/refs/heads/main/fuchsia/time/metrics.yaml#751

import sys
import collections
import math
import matplotlib.pyplot as plt
import statsmodels.api as sm

class Distribution:
    def __init__(self, bins, bin_width):
        self.bins = bins
        self.bin_width = bin_width
        self.normalize()
    
    def normalize(self):
        total = sum(self.bins)
        self.bins = [ q / total for q in self.bins]
    
    def conflate(self, other, bin_offset):
        if len(self.bins) != len(other.bins):
            raise "distributions have different number of bins"
        if self.bin_width != other.bin_width:
            raise "distributions have different bin width"
        
        new_bins = []
        for idx in range(0, len(self.bins) + bin_offset):
            idx_in_self = idx - bin_offset
            idx_in_other = idx
            if idx_in_self < 0 or idx_in_other >= len(other.bins):
                continue
            new_bins.append(self.bins[idx_in_self]*other.bins[idx_in_other])
        
        return Distribution(new_bins, self.bin_width)
    
    def standard_deviation(self):
        # here we add .5 to indices to treat the values as if they occur at the
        # center of each bin.
        # first find mean pretending each bin is 1 unit wide
        mean_numerator = sum(
            [(idx + 0.5)*quantity for (idx, quantity) in enumerate(self.bins)]
        )
        mean_denominator = sum(self.bins)
        idx_mean = mean_numerator / mean_denominator

        variance = 0
        for (idx, quantity) in enumerate(self.bins):
            dist_to_mean = ((idx + 0.5) - idx_mean)*self.bin_width
            variance += quantity * dist_to_mean ** 2
        
        return math.sqrt(variance)

HistogramEntry = collections.namedtuple('HistogramEntry', ['rtt_bucket', 'offset_bucket', 'bucket_count'])
RTT_BUCKET_WIDTH_MS = 10
OFFSET_BUCKET_WIDTH_MS=10

def cobalt_raw_entry_to_entry(line):
    # See https://fuchsia.googlesource.com/cobalt-registry/+/refs/heads/main/fuchsia/time/metrics.yaml#751
    # Input is a slightly modified version - our query to retrieve the data removes overflow and underflow
    # values and combines the bucket_index and Postive/Negative sign
    if "null" in line:
        return None
    entries = line.split(',')
    if len(entries) != 3:
        return None
    cobalt_offset_bucket = int(entries[2])
    # overflow and underflow is already discarded. {0, 101, -101}
    # So the cobalt representation contains [-100, -1] and [1, 100] (note no 0 value)
    # For this script we coerce to [0, 199]
    # bucket representation in this script starts with 0 for convenience
    # in this script index 100 is offset in range [0, OFFSET_BUCKET_WIDTH_MS), 99 is [-OFFSET_BUCKET_WIDTH_MS, 0), ...
    offset_bucket = cobalt_offset_bucket + 100 if cobalt_offset_bucket < 0 else cobalt_offset_bucket + 99 
    # cobalt representation is from [1, 100] with overflow discarded {0, 100}
    # for convenience make this start on 0.
    rtt_bucket = int(entries[0]) - 1
    bucket_count = int(entries[1])

    return HistogramEntry(rtt_bucket, offset_bucket, bucket_count)

def sd_by_rtt(dist_by_rtt_bucket):
    rtt_sd = [(bucket, dist.standard_deviation()) for (bucket, dist) in dist_by_rtt_bucket.items()]
    rtt_sd.sort()

    fig = plt.figure()
    ax = fig.add_axes([0,0,1,1])
    bucket_names = [str(bucket_no) for (bucket_no, v) in rtt_sd]
    bucket_counts = [v for (k, v) in rtt_sd]
    ax.plot(bucket_names, bucket_counts)
    plt.show()

def plot_dist_cmp(dist_by_rtt_bucket, buckets):
    COLORS = ['red', 'green', 'blue', 'orange']
    distributions = [dist_by_rtt_bucket[bucket] for bucket in buckets]

    bucket_names = [(i - 100)*OFFSET_BUCKET_WIDTH_MS for i in range(0, 200)]

    def label_for_rtt(rtt_bucket):
        return 'RTT [{},{})'.format(rtt_bucket*RTT_BUCKET_WIDTH_MS, (rtt_bucket + 1)*RTT_BUCKET_WIDTH_MS)
    dist_labels = [label_for_rtt(rtt_bucket) for rtt_bucket in buckets]

    fig = plt.figure()
    ax = fig.add_axes([0,0,1,1])
    for (distribution, name, color) in zip(distributions, dist_labels, COLORS):
        ax.plot(bucket_names, distribution.bins, color=color, label=name)
    ax.axvline(0)
    ax.legend()
    
    plt.show()

def regression(dist_by_rtt_bucket):
    DeviationEntry = collections.namedtuple('DeviationEntry', ['rtt_bucket_1', 'rtt_bucket_2', 'offset_bucket', 'sd'])
    deviation_entries = []
    for (rtt_bucket_1, dist_1) in dist_by_rtt_bucket.items():
        for (rtt_bucket_2, dist_2) in dist_by_rtt_bucket.items():
            for offset_bucket in range(1, 100):
                conflated_dist = dist_1.conflate(dist_2, offset_bucket)
                sd = conflated_dist.standard_deviation()
                deviation_entries.append(DeviationEntry(rtt_bucket_1=rtt_bucket_1, rtt_bucket_2=rtt_bucket_2, offset_bucket=offset_bucket, sd = sd))
    
    # this is a bit hard to visualize because 3 dimensions
    # we'll show two graphs at fixed offsets to try to demonstrate
    sd_at_offset_250 = [[0 for i in range(100)] for j in range(100)]
    for entry in deviation_entries:
        if entry.offset_bucket == 25:
            sd_at_offset_250[entry.rtt_bucket_1][entry.rtt_bucket_2] = entry.sd
    
    sd_at_offset_500 = [[0 for i in range(100)] for j in range(100)]
    for entry in deviation_entries:
        if entry.offset_bucket == 50:
            sd_at_offset_500[entry.rtt_bucket_1][entry.rtt_bucket_2] = entry.sd
    
    sd_at_offset_750 = [[0 for i in range(100)] for j in range(100)]
    for entry in deviation_entries:
        if entry.offset_bucket == 75:
            sd_at_offset_750[entry.rtt_bucket_1][entry.rtt_bucket_2] = entry.sd

    all_sds = [entry.sd for entry in deviation_entries]
    def as_var_tuple(entry):
        rtt_bucket_1_val = (entry.rtt_bucket_1 + 0.5) * RTT_BUCKET_WIDTH_MS
        rtt_bucket_2_val = (entry.rtt_bucket_2 + 0.5) * RTT_BUCKET_WIDTH_MS
        offset = entry.offset_bucket*OFFSET_BUCKET_WIDTH_MS
        return (rtt_bucket_1_val, rtt_bucket_2_val, offset)
    vars = [as_var_tuple(entry) for entry in deviation_entries]
    vars = sm.add_constant(vars)

    model = sm.OLS(all_sds, vars).fit()
    print(model.summary())

    min_sd = min(all_sds)
    max_sd = max(all_sds)

    fig = plt.figure()
    ax_1 = fig.add_subplot(2,2,1,label="250")
    ax_1.imshow(sd_at_offset_250, vmin=min_sd, vmax=max_sd)
    ax_1.set_title("SD when polls are 250 ms offset")

    ax_1 = fig.add_subplot(2,2,2,label="500")
    ax_1.imshow(sd_at_offset_500, vmin=min_sd, vmax=max_sd)
    ax_1.set_title("SD when polls are 500 ms offset")

    ax_2 = fig.add_subplot(2,2,3,label="750")
    ax_2.imshow(sd_at_offset_750, vmin=min_sd, vmax=max_sd)
    ax_2.set_title("SD when polls are 750 ms offset")

    plt.show()
    


if __name__ == "__main__":
    histogram_entries_by_rtt_bucket = collections.defaultdict(list)
    for line in sys.stdin:
        # skip first line
        if "bucket_count" in line:
            continue
        new_entry = cobalt_raw_entry_to_entry(line)
        if new_entry is not None:
            histogram_entries_by_rtt_bucket[new_entry.rtt_bucket].append(new_entry)
    dist_by_rtt_bucket = {}
    for (rtt_bucket, entries) in histogram_entries_by_rtt_bucket.items():
        counts_by_offset = { entry.offset_bucket : entry.bucket_count for entry in entries}
        bins = [counts_by_offset[i] for i in range(0, 200)]
        dist_by_rtt_bucket[rtt_bucket] = Distribution(bins, OFFSET_BUCKET_WIDTH_MS)
    
    if len(sys.argv) < 2:
        print("Need to specify a command")
    elif sys.argv[1] == 'sd-by-rtt':
        sd_by_rtt(dist_by_rtt_bucket)
    elif sys.argv[1] == 'plot-dist-cmp':
        buckets = [int(arg) for arg in sys.argv[2:]]
        plot_dist_cmp(dist_by_rtt_bucket, buckets)
    elif sys.argv[1] == 'regression':
        regression(dist_by_rtt_bucket)
