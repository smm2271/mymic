"""Offline evidence analysis of the existing diagnostics JSONL; no audio I/O.

Every report is scoped by clock/session/stream. Percentiles use nearest rank.
Network starvation means authenticated RX admission starvation, not proven Wi-Fi loss.
"""
import argparse
import json
import math
from collections import Counter, defaultdict
from pathlib import Path


def stats(values, population):
    values = sorted(v for v in values if v is not None)
    return dict(population=population, count=len(values), collected=bool(values),
                p50=values[math.ceil(len(values)*.50)-1] if values else None,
                p95=values[math.ceil(len(values)*.95)-1] if values else None,
                p99=values[math.ceil(len(values)*.99)-1] if values else None,
                max=max(values) if values else None,
                min=min(values) if values else None)


def total(rows, field):
    values = [r.get(field) for r in rows]
    return sum(values) if values and all(v is not None for v in values) else None


def candidate(event, rows, health):
    end = event['rx_monotonic_us']
    start = max(0, end-500_000)
    prior = [r for r in rows if start <= r['rx_monotonic_us'] <= end]
    packets = [r for r in prior if r.get('record_type') == 'udp_receive']
    jitter = [r for r in prior if r.get('record_type') == 'jitter_dequeue']
    callbacks = [r for r in prior if r.get('record_type') == 'audio_callback']
    h = [r for r in health if start-150_000 <= r['rx_monotonic_us'] <= end+150_000]
    evidence = []
    candidates = []
    missing = []
    def add(category, row, fact):
        candidates.append(category)
        evidence.append(dict(category=category, rx_monotonic_us=row['rx_monotonic_us'],
                             packet_sequence=row.get('packet_sequence'), fact=fact))
    # Coverage is explicit. A log starting at a symptom cannot prove preceding silence.
    clean = (h and h[0]['rx_monotonic_us'] <= start+100_000
             and h[-1]['rx_monotonic_us'] >= end-100_000
             and all(r['enabled'] and not r.get('log_error') for r in h)
             and len({(r['event_drops'], r['observation_drops'], r.get('enabled_changes')) for r in h}) == 1
             and max((b['rx_monotonic_us']-a['rx_monotonic_us'] for a, b in zip(h, h[1:])), default=0) <= 200_000)
    if not clean:
        missing.append('complete enabled diagnostic health coverage without queue drops/log errors')
    if not packets:
        missing.append('authenticated UDP arrivals in preceding window')
    if not jitter:
        missing.append('jitter dequeue observations')
    if not callbacks:
        missing.append('callback observations (output may be unopened/headless)')
    delayed = [r for r in callbacks if r.get('started') and r.get('interval_us') is not None
               and r.get('budget_us') is not None and r['interval_us'] > r['budget_us']+20_000]
    for r in delayed:
        add('Scheduling Delay', r, f"callback interval {r['interval_us']} us > previous budget {r['budget_us']} us + 20000 us")
        add('Audio Callback Starvation', r, 'active callback invocation missed its observed cadence; OS scheduling cause unproven')
    for r in prior:
        if r.get('reason') == 'receiver_scheduler_stall':
            add('Scheduling Delay', r, 'receiver tick observed >20 ms overdue; not a network measurement')
    gaps = [r for r in prior if r.get('sequence_gaps') is not None]
    gap_delta = max(0, gaps[-1]['sequence_gaps']-gaps[0]['sequence_gaps']) if gaps else None
    if clean and packets:
        tail = end-packets[-1]['rx_monotonic_us']
        late = [r for r in packets if (r.get('interval_us') or 0) > 15_000]
        if tail > 15_000:
            add('Network Starvation', event, f'last authenticated RX arrival precedes symptom by {tail} us; TX/socket/network not separable')
        for r in late:
            add('Network Starvation', r, f"authenticated RX inter-arrival {r['interval_us']} us > 15000 us; TX/socket/network not separable")
        if gap_delta:
            add('Network Starvation', gaps[-1], f'{gap_delta} sequence holes finalized by existing 40 ms tracker in window; not proof of wire loss')
    if event.get('event_type') == 'BUFFER_UNDERRUN' and jitter:
        add('Jitter Buffer Starvation', event,
            f"expected packet {event.get('expected_sequence')} absent at deadline; occupancy {event.get('jitter_buffer_depth')}; may contain future packets")
    for r in callbacks:
        if (r.get('missing_input_samples') or 0) > 0:
            add('Playback Ring Starvation', r,
                f"{r['missing_input_samples']} empty resampler input attempts; ring entry {r.get('ring_before')} exit {r.get('ring_after')}, {r.get('starvation_frames')} device frames inserted after starvation")
    if missing:
        candidates.append('Unknown / Insufficient Evidence')
    if not candidates:
        candidates.append('Unknown / Insufficient Evidence')
        missing.append('no measured antecedent supports a trigger; event name alone insufficient')
    evidence.sort(key=lambda r: r['rx_monotonic_us'])
    depths = [r['occupancy'] for r in jitter]
    pending_holes = [r['pending_sequence_holes'] for r in prior if r.get('pending_sequence_holes') is not None]
    follow_up = [r for r in rows if end < r['rx_monotonic_us'] <= end+500_000
                 and event.get('expected_sequence') is not None
                 and r.get('packet_sequence') == event['expected_sequence']
                 and (r.get('record_type') == 'udp_receive' or r.get('event_type') == 'LATE_PACKET')]
    falling = len(depths) >= 3 and depths[-1] < depths[0] and all(b <= a for a, b in zip(depths, depths[1:]))
    return dict(record_type='root_cause_candidates', window_start_us=start, window_end_us=end,
                event=event, candidates=list(dict.fromkeys(candidates)), evidence=evidence,
                missing_observations=missing, sequence_gap_delta=gap_delta,
                pending_sequence_holes_max=max(pending_holes) if pending_holes else None,
                expected_sequence_received_in_window=any(r.get('packet_sequence') == event['expected_sequence'] for r in packets)
                    if event.get('expected_sequence') is not None and packets else None,
                expected_sequence_follow_up_500ms=follow_up,
                udp_arrivals=len(packets), udp_normal_observed=(clean and bool(packets) and
                    end-packets[-1]['rx_monotonic_us'] <= 15_000 and
                    all((r.get('interval_us') or 0) <= 15_000 for r in packets) and gap_delta == 0 and
                    not any(pending_holes)) if clean and packets and gaps else None,
                jitter_occupancy_monotonically_falling=falling if len(depths) >= 3 else None,
                resync_or_recovery=[r for r in prior if r.get('event_type') in ('BUFFER_RESYNC', 'AUDIO_RECOVERY')],
                earliest_observed_evidence=evidence[0] if evidence else None,
                interpretation='candidates may coexist; temporal precedence is not causal proof')


def analyze(records, metadata=None):
    groups = defaultdict(list)
    for r in records:
        if 'rx_monotonic_us' in r:
            groups[(r.get('clock_id'), r.get('session_id'), r.get('stream_id'))].append(r)
    output = []
    for (clock, session, stream), rows in groups.items():
        if metadata and (clock, session, stream) != (metadata['clock_id'], metadata['session_id'], metadata['stream_id']):
            continue
        rows.sort(key=lambda r: r['rx_monotonic_us'])
        health = [r for r in rows if r.get('record_type') == 'diagnostic_health']
        start = metadata['started_monotonic_us'] if metadata else rows[0]['rx_monotonic_us']
        end = metadata['ended_monotonic_us'] if metadata else rows[-1]['rx_monotonic_us']
        selected = [r for r in rows if start <= r['rx_monotonic_us'] <= end]
        if not selected:
            continue
        identity = dict(clock_id=clock, session_id=session, stream_id=stream)
        packets = [r for r in selected if r.get('record_type') == 'udp_receive']
        callbacks = [r for r in selected if r.get('record_type') == 'audio_callback']
        jitter = [r for r in selected if r.get('record_type') == 'jitter_dequeue']
        enqueue_times = {}
        residence = []
        for r in rows:
            if r.get('record_type') in ('stream_reset', 'stream_start'):
                enqueue_times.clear()
            if r.get('record_type') == 'jitter_enqueue' and r.get('admitted'):
                enqueue_times[r['packet_sequence']] = r['rx_monotonic_us']
            if r.get('record_type') == 'jitter_dequeue':
                entered = enqueue_times.pop(r.get('expected_sequence'), None)
                if r.get('delivered') and entered is not None and start <= r['rx_monotonic_us'] <= end:
                    residence.append(r['rx_monotonic_us']-entered)
            # No unbounded stale history across capacity eviction / rejected packets.
            if len(enqueue_times) > 256:
                enqueue_times.pop(next(iter(enqueue_times)))
        sequence = [r['sequence_gaps'] for r in rows if r.get('sequence_gaps') is not None and r['rx_monotonic_us'] <= end]
        baseline = [r['sequence_gaps'] for r in rows if r.get('sequence_gaps') is not None and r['rx_monotonic_us'] < start]
        summary = dict(record_type='session_summary', **identity, window_start_us=start, window_end_us=end,
            window_semantics='inclusive RX monotonic timestamps; admitted records only',
            packet_population='authenticated active-session packets after decode; duplicates included; kernel arrival and rejected datagrams not traced',
            packets=dict(count=len(packets), collected=bool(packets), duplicates=total(packets, 'duplicate'),
                         reordered=total(packets, 'out_of_order'),
                         finalized_gap_delta=sequence[-1]-(baseline[-1] if baseline else sequence[0]) if sequence else None),
            inter_arrival_us=stats([r.get('interval_us') for r in packets], 'adjacent authenticated arrivals, attributed to ending arrival; may cross window start'),
            jitter_occupancy_packets=stats([r.get('occupancy') for r in jitter], 'post-dequeue depth including prefill ticks'),
            jitter_residence_us=stats(residence, 'successfully delivered sequences matched to last admitted enqueue within stream; ending dequeue in window'),
            jitter_capacity_packets=sorted({r['capacity'] for r in selected if r.get('record_type') in ('jitter_enqueue', 'jitter_dequeue') and r.get('capacity') is not None}),
            event_counts=dict(Counter(r['event_type'] for r in selected if r.get('event_type'))),
            callback_interval_us=stats([r.get('interval_us') for r in callbacks], 'adjacent CPAL entries including idle; attributed to ending entry'),
            playback_ring_samples=stats([r.get('ring_before') for r in callbacks], 'callback entry 16 kHz samples'),
            callback_requested_frames=total(callbacks, 'requested_frames'),
            callback_supplied_frames=total(callbacks, 'supplied_frames'),
            inserted_device_frames=total(callbacks, 'inserted_frames'),
            starvation_device_frames=total(callbacks, 'starvation_frames'),
            prefill_device_frames=total(callbacks, 'prefill_frames'),
            inactive_device_frames=total(callbacks, 'inactive_frames'),
            diagnostic_health=health, limitations=['no Windows physical/loopback observation', 'TX vs network vs receiver socket delay unresolved',
                'sequence gaps require a later higher sequence; trailing lost packets cannot be enumerated', 'absence requires health coverage; disk cap/crash can truncate log'])
        summary['capture_diagnostic_status'] = {k: metadata.get(k) for k in (
            'event_drop_count_start', 'event_drop_count_end', 'observation_drop_count_start',
            'observation_drop_count_end', 'event_log_error', 'error', 'reason')} if metadata else None
        summary['observed_range_us'] = [selected[0]['rx_monotonic_us'], selected[-1]['rx_monotonic_us']]
        summary['event_count_semantics'] = 'absent keys mean no admitted event records, not proof of no event occurrence'
        output.append(summary)
        for r in selected:
            if r.get('event_type'):
                output.append({**r, 'record_type': 'event_timeline'})
                if r['event_type'] in ('BUFFER_UNDERRUN', 'AUDIO_CALLBACK_STARVATION', 'AUDIO_CALLBACK_DELAY'):
                    output.append(dict(**identity, **candidate(r, rows, health)))
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('jsonl', type=Path)
    parser.add_argument('--capture', type=Path, help='WAV sidecar; restrict summary to capture and retain 500 ms prehistory')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    records = []
    paths = [args.jsonl]
    retention = None
    fault_path = None
    if args.jsonl.suffix == '.json':
        manifest = json.loads(args.jsonl.read_text(encoding='utf8'))
        paths = [Path(part['path']) for part in manifest['detail_parts']]
        retention = manifest.get('detail_evicted_through_us')
        fault_path = Path(manifest['fault_archive_path']) if 'fault_archive_path' in manifest else None
    lines = (line for path in paths for line in path.read_text(encoding='utf8').splitlines())
    for number, line in enumerate(lines, 1):
        if line.strip():
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError as e:
                raise SystemExit(f'invalid/truncated JSONL line {number}: {e}') from e
    if fault_path and fault_path.exists():
        # Multiset dedup only the archive copies; retain genuinely repeated events.
        copies = Counter(json.dumps(r, sort_keys=True) for r in records if r.get('event_type'))
        for line in fault_path.read_text(encoding='utf8').splitlines():
            if not line.strip():
                continue
            row = json.loads(line); key = json.dumps(row, sort_keys=True)
            if copies[key]:
                copies[key] -= 1
            else:
                records.append(row)
    metadata = json.loads(args.capture.read_text(encoding='utf8')) if args.capture else None
    result = analyze(records, metadata)
    for row in result:
        if row.get('record_type') == 'session_summary':
            row['detail_evicted_through_us'] = retention
            row['detail_retention_incomplete'] = retention is not None and row['window_start_us'] <= retention
    if not result:
        raise SystemExit('no matching session/window observations; no zero-loss claim can be made')
    args.output.write_text(''.join(json.dumps(r, ensure_ascii=False)+'\n' for r in result), encoding='utf8')
    print(f'{len(result)} report records written to {args.output}')


if __name__ == '__main__':
    main()
