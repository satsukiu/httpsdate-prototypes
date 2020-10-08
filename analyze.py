#!/usr/bin/python3

import sys
import collections
import math

Sample = collections.namedtuple('Sample', ['polls', 'bound_size', 'delta_avg', 'delta_max', 'error'])

NUM_BINS = 25
BOUND_BIN_SIZE = 50_000_000

NANOS_IN_SECS = 1_000_000_000.

samples = []
for line in sys.stdin:
    raw = line.split(',')
    if raw[0].startswith('polls'):
        continue
    sample = Sample(polls=int(raw[0]), \
        bound_size=int(raw[1]), \
        delta_avg=int(raw[2]),
        delta_max=int(raw[3]),
        error=int(raw[4]))
    samples.append(sample)

bins_by_bound_size = [[] for _ in range(NUM_BINS)]
for sample in samples:
    bin_no = sample.bound_size // BOUND_BIN_SIZE
    bins_by_bound_size[bin_no].append(sample.error)

datapoints = []
for bin_no, bin in enumerate(bins_by_bound_size):
    if len(bin) > 0:
        sigma = math.sqrt(sum([x*x for x in bin])/len(bin))
        print(bin_no*BOUND_BIN_SIZE/NANOS_IN_SECS, len(bin), sigma/NANOS_IN_SECS)

