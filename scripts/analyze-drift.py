"""Stage A apparent-rate estimates from bounded 1 Hz summaries. No clock control."""
import argparse
import json
import math
from collections import deque
from pathlib import Path


def slope(points):
    if len(points) < 3:
        return None
    xs = [p[0] for p in points]; ys = [p[1] for p in points]
    x = sum(xs)/len(xs); y = sum(ys)/len(ys)
    xx = sum((v-x)**2 for v in xs)
    if not xx:
        return None
    b = sum((a-x)*(v-y) for a, v in points)/xx
    rms = math.sqrt(sum((v-y-b*(a-x))**2 for a, v in points)/len(points))
    return dict(slope=b, residual_rms=rms, count=len(points), duration_s=xs[-1]-xs[0])


def merge(rows):
    result = dict(rows[0]); result['statistics'] = {}; result['counts'] = {}; result['invalid_reasons'] = []
    outputs = set()
    for row in rows:
        if row.get('output_epoch') is not None:
            outputs.add(row['output_epoch'])
        result['invalid_reasons'] += row['invalid_reasons']
        if row['discarded_summary_buckets_total']:
            result['invalid_reasons'].append('summary_bucket_overflow')
        for key, value in row['statistics'].items():
            a = result['statistics'].setdefault(key, dict(count=0, sum=0, min=value['min'], max=value['max']))
            a['count'] += value['count']; a['sum'] += value['sum']; a['min'] = min(a['min'], value['min']); a['max'] = max(a['max'], value['max'])
        for key, value in row['counts'].items():
            result['counts'][key] = result['counts'].get(key, 0)+value
        if row.get('source_last'):
            result['source_last'] = row['source_last']
        if row.get('source_envelope'):
            result['source_envelope'] = row['source_envelope']
    if len(outputs) > 1:
        result['invalid_reasons'].append('multiple_output_epochs')
    result['output_epoch'] = next(iter(outputs)) if len(outputs) == 1 else None
    for value in result['statistics'].values():
        value['mean'] = value['sum']/value['count']
    return result


def estimate(rows, window):
    reasons = sorted({reason for r in rows for reason in r['invalid_reasons']})
    span = (rows[-1]['window_end_us']-rows[0]['window_start_us'])/1e6
    if len({r['window_start_us'] for r in rows}) != len(rows) or span != len(rows):
        reasons.append('missing_or_duplicate_summary_seconds')
    if span < window:
        reasons.append('window_not_collected')
    points = []; previous = None; unwrapped = 0
    for row in rows:
        p = row.get('source_envelope') or row.get('source_last')
        if p:
            source = p['source_sample_index']
            if previous is not None:
                delta = (source-previous) & 0xffffffff
                if delta >= 0x80000000:
                    reasons.append('source_timestamp_discontinuity'); break
                unwrapped += delta
            previous = source
            points.append((p['rx_monotonic_us']/1e6, unwrapped))
    source = slope(points)
    if source is None or source['duration_s'] < window-2:
        reasons.append('source_points_insufficient')
    def values(field):
        return [r['statistics'][field] for r in rows if field in r['statistics']]
    def rate(field):
        v = values(field)
        return sum(x['sum'] for x in v)/span if len(v) == len(rows) else None
    source_valid = not reasons
    device = rate('audio_callback.requested_frames')
    if device is None or device <= 0:
        reasons.append('device_request_population_incomplete')
    rates = values('audio_callback.output_rate')
    config = rates[0]['mean'] if rates else None
    if not rates or len(rates) != len(rows) or any(v['min'] != config or v['max'] != config for v in rates):
        reasons.append('output_format_or_callback_coverage_incomplete'); config = None
    def trend(field):
        return slope([(r['window_start_us']/1e6, r['statistics'][field]['mean']) for r in rows if field in r['statistics']])
    valid = not reasons
    return dict(record_type='drift_estimate', clock_id=rows[-1]['clock_id'], session_id=rows[-1]['session_id'],
                stream_id=rows[-1]['stream_id'], playout_epoch=rows[-1].get('playout_epoch'), output_epoch=rows[-1].get('output_epoch'),
                requested_window_seconds=window, window_start_us=rows[0]['window_start_us'], window_end_us=rows[-1]['window_end_us'],
                valid=valid, source_host_valid=source_valid, invalid_reasons=sorted(set(reasons)), confidence='apparent_only_network_and_dispatch_delay_unidentifiable' if valid else 'insufficient_evidence',
                source_rate_samples_s=source['slope'] if source else None,
                source_point_fit=source, device_effective_frames_s=device, input_consumed_samples_s=rate('audio_callback.consumed_input_samples'),
                source_host_ppm=1e6*(source['slope']/16000-1) if source_valid else None,
                device_host_ppm=1e6*(device/config-1) if valid else None,
                source_device_ppm=1e6*((source['slope']/16000)/(device/config)-1) if valid and device else None,
                jitter_occupancy_packets_trend=trend('jitter_dequeue.occupancy'),
                ring_entry_samples_trend=trend('audio_callback.ring_before'),
                population='admitted per-second observations; source slope uses one lower-envelope unique in-order header point per second; fallback last-point for old summaries; occupancy means observation-weighted')


def analyze(rows):
    # Writer rows arrive in finalized-second order. Bound active histories, not run duration.
    histories = {}; pending = []; second = None
    def finish(batch):
        groups = {}
        for r in batch:
            key = (r['clock_id'], r['session_id'], r['stream_id'], r.get('playout_epoch'))
            # Health/event records have no playout ownership. Their invalid flags were
            # already propagated by the worker to all buckets for the second.
            if key[-1] is not None:
                groups.setdefault(key, []).append(r)
        output = []
        for key, group in groups.items():
            row = merge(group)
            history_key = key+(row.get('output_epoch'),)
            if history_key not in histories and len(histories) >= 16:
                histories.pop(next(iter(histories)))
            history = histories.setdefault(history_key, deque(maxlen=120)); history.append(row)
            for window in (10, 30, 60, 120):
                output.append(estimate(list(history)[-window:], window))
        return output
    for r in rows:
        if r.get('record_type') != 'drift_summary':
            continue
        if second is not None and r['window_start_us'] != second:
            yield from finish(pending); pending = []
        second = r['window_start_us']; pending.append(r)
        if len(pending) > 64:
            raise ValueError('summary second exceeded bounded identity population')
    if pending:
        yield from finish(pending)


def main():
    p = argparse.ArgumentParser(description=__doc__); p.add_argument('input', type=Path); p.add_argument('--output', required=True, type=Path)
    args = p.parse_args(); path = args.input
    if path.suffix == '.json':
        path = Path(json.loads(path.read_text(encoding='utf8'))['summary_path'])
    with path.open(encoding='utf8') as source, args.output.open('w', encoding='utf8') as destination:
        for row in analyze(json.loads(line) for line in source if line.strip()):
            destination.write(json.dumps(row)+'\n')


if __name__ == '__main__':
    main()
