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
import statsmodels.api as sm

Sample = collections.namedtuple('Sample', ['polls', 'bound_size', 'delta_avg', 'delta_max', 'error'])

NUM_BOUND_BINS = 24
NUM_DELTA_BINS = 30
BOUND_BIN_SIZE = 50_000_000
DELTA_BIN_SIZE = 2_000_000

NANOS_IN_SECS = 1_000_000_000.
NANOS_IN_MILLIS = 1_000_000.

def sigma(errors):
    if len(errors) <= 5:
        return 0
    else:
        return math.sqrt(sum([x*x for x in errors])/len(errors))

def regression(bin_sigmas):
    data = []
    for (bound_idx, bound_bin) in enumerate(bin_sigmas):
        for (delta_idx, sigma) in enumerate(bound_bin):
            if sigma > 0:
                data.append(((delta_idx + 0.5)*DELTA_BIN_SIZE/NANOS_IN_MILLIS, (bound_idx+0.5)*BOUND_BIN_SIZE/NANOS_IN_MILLIS, sigma/NANOS_IN_MILLIS))
    ind = [d[1] for d in data]# [(d[0], d[1]) for d in data]
    sigmas = [d[2] for d in data]

    # ind = sm.add_constant(ind)

    model = sm.OLS(sigmas, ind).fit()
    print(model.summary())

def heatmap(bin_sigmas):
    fig, ax = plt.subplots()
    im = ax.imshow(bin_sigmas)

    ax.set_yticks(np.arange(NUM_BOUND_BINS))
    ax.set_xticks(np.arange(NUM_DELTA_BINS))
    ax.set_ylabel('bound size ms')
    ax.set_xlabel('avg delta size ms')
    ax.set_yticklabels([int(x*BOUND_BIN_SIZE/NANOS_IN_MILLIS) for x in range(NUM_BOUND_BINS)])
    ax.set_xticklabels([int(x*DELTA_BIN_SIZE/NANOS_IN_MILLIS) for x in range(NUM_DELTA_BINS)])
    plt.setp(ax.get_xticklabels(), rotation=45, ha="right", rotation_mode="anchor")

    for i in range(NUM_DELTA_BINS):
        for j in range(NUM_BOUND_BINS):
            if bin_sigmas[j][i] > 0:
                text = ax.text(i, j, int(bin_sigmas[j][i]/NANOS_IN_MILLIS), ha="center", va="center", color="w")

    ax.set_title("Sigma in ms")

    plt.show()


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

bound_delta_bins = [[[] for _ in range(NUM_DELTA_BINS)] for _ in range(NUM_BOUND_BINS)]
for sample in samples:
    delta_bin = sample.delta_avg // DELTA_BIN_SIZE
    bound_bin = sample.bound_size // BOUND_BIN_SIZE
    if delta_bin < NUM_DELTA_BINS and bound_bin < NUM_BOUND_BINS:
        bound_delta_bins[bound_bin][delta_bin].append(sample.error)

bin_sigmas = [[sigma(delta_bin) for delta_bin in bound_bin] for bound_bin in bound_delta_bins]

regression(bin_sigmas)
heatmap(bin_sigmas)