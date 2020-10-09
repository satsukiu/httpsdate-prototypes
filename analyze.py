#!/usr/bin/python3

# A script that ingests data produced from the 'httpdate' CLI. This currently
# does little but will eventually derive some relationship between sample error
# distribution and the other recorded data.

import sys
import collections
import math
import numpy as np
import matplotlib
import matplotlib.pyplot as plt

Sample = collections.namedtuple('Sample', ['polls', 'bound_size', 'delta_avg', 'delta_max', 'error'])

NUM_BINS = 12
BOUND_BIN_SIZE = 100_000_000
DELTA_BIN_SIZE = 2_000_000

NANOS_IN_SECS = 1_000_000_000.
NANOS_IN_MILLIS = 1_000_000

def sigma(errors):
    if len(errors) == 0:
        return 0
    else:
        return math.sqrt(sum([x*x for x in errors])/len(errors))

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

delta_bound_bins = [[[] for _ in range(NUM_BINS)] for _ in range(NUM_BINS)]
for sample in samples:
    delta_bin = sample.delta_avg // DELTA_BIN_SIZE
    bound_bin = sample.bound_size // BOUND_BIN_SIZE
    if delta_bin < NUM_BINS and bound_bin < NUM_BINS:
        delta_bound_bins[delta_bin][bound_bin].append(sample.error)

bin_sigmas = [[sigma(bound_bin)/NANOS_IN_SECS for bound_bin in delta_bin] for delta_bin in delta_bound_bins]
fig, ax = plt.subplots()
im = ax.imshow(bin_sigmas)

ax.set_xticks(np.arange(NUM_BINS))
ax.set_yticks(np.arange(NUM_BINS))
ax.set_xlabel('bound size ms')
ax.set_ylabel('avg delta size ms')
ax.set_xticklabels([int(x*BOUND_BIN_SIZE/NANOS_IN_MILLIS) for x in range(NUM_BINS)])
ax.set_yticklabels([int(x*DELTA_BIN_SIZE/NANOS_IN_MILLIS) for x in range(NUM_BINS)])
plt.setp(ax.get_xticklabels(), rotation=45, ha="right", rotation_mode="anchor")

for i in range(NUM_BINS):
    for j in range(NUM_BINS):
        if bin_sigmas[i][j] > 0:
            text = ax.text(j, i, int(1000*bin_sigmas[i][j]), ha="center", va="center", color="w")

ax.set_title("Sigma in ms")

plt.show()