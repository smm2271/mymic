"""Deterministic timeline fixtures; complements tests using actual Jitter/renderer."""
import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location('analysis', Path(__file__).resolve().parents[1] / 'scripts/analyze-continuity.py')
analysis = importlib.util.module_from_spec(spec)
spec.loader.exec_module(analysis)


def row(kind, time, **values):
    return dict(record_type=kind, rx_monotonic_us=time, clock_id='clock', session_id='session', stream_id=1, **values)


def normal():
    rows = [row('diagnostic_health', t, enabled=True, event_drops=0, observation_drops=0, log_error=None)
            for t in range(0, 700001, 100000)]
    for i in range(61):
        rows += [row('udp_receive', i*10000, packet_sequence=i, interval_us=10000 if i else None,
                     sequence_gaps=0, duplicate=False, out_of_order=False),
                 row('jitter_dequeue', i*10000, occupancy=2, delivered=True),
                 row('audio_callback', i*10000, started=True, interval_us=10000 if i else None,
                     budget_us=10000, requested_frames=480, supplied_frames=480,
                     inserted_frames=0, missing_input_samples=0, ring_before=320, ring_after=160)]
    return sorted(rows, key=lambda r: r['rx_monotonic_us'])


def event(kind='BUFFER_UNDERRUN'):
    return row('event', 600000, event_type=kind, expected_sequence=60, jitter_buffer_depth=0, reason='test')


def classify(rows, e=None):
    return analysis.candidate(e or event(), rows, [r for r in rows if r['record_type'] == 'diagnostic_health'])


class TimelineTests(unittest.TestCase):
    def test_normal_no_false_network_starvation(self):
        result = classify(normal())
        self.assertTrue(result['udp_normal_observed'])
        self.assertNotIn('Network Starvation', result['candidates'])
        self.assertIn('Jitter Buffer Starvation', result['candidates'])

    def test_single_and_contiguous_loss(self):
        for count in (1, 4):
            rows = normal()
            for r in rows:
                if r['rx_monotonic_us'] >= 550000 and r.get('sequence_gaps') is not None:
                    r['sequence_gaps'] = count
            result = classify(rows)
            self.assertEqual(result['sequence_gap_delta'], count)
            self.assertIn('Network Starvation', result['candidates'])

    def test_delay_and_late_evidence(self):
        rows = normal()
        next(r for r in rows if r['record_type'] == 'udp_receive' and r['rx_monotonic_us'] == 500000)['interval_us'] = 30000
        result = classify(rows)
        self.assertFalse(result['udp_normal_observed'])
        self.assertIn('Network Starvation', result['candidates'])
        self.assertEqual(result['earliest_observed_evidence']['rx_monotonic_us'], 500000)

    def test_reorder_and_duplicate_not_loss(self):
        rows = normal()
        packets = [r for r in rows if r['record_type'] == 'udp_receive']
        packets[20]['out_of_order'] = True
        packets[21]['duplicate'] = True
        summary = analysis.analyze(rows)[0]
        self.assertEqual(summary['packets']['reordered'], 1)
        self.assertEqual(summary['packets']['duplicates'], 1)
        self.assertEqual(summary['packets']['finalized_gap_delta'], 0)

    def test_burst_preserves_individual_events(self):
        rows = normal()
        for n in range(8):
            rows.append(row('event', 500000+n, event_type='BUFFER_RESYNC', reason='jitter_capacity_reject_farther'))
        self.assertEqual(sum(r['record_type'] == 'event_timeline' for r in analysis.analyze(rows)), 8)

    def test_callback_delay_precedes_ring_starvation(self):
        rows = normal()
        callback = next(r for r in rows if r['record_type'] == 'audio_callback' and r['rx_monotonic_us'] == 550000)
        callback.update(interval_us=40000, missing_input_samples=1, starvation_frames=100)
        result = classify(rows, event('AUDIO_CALLBACK_STARVATION'))
        self.assertIn('Audio Callback Starvation', result['candidates'])
        self.assertIn('Scheduling Delay', result['candidates'])
        self.assertIn('Playback Ring Starvation', result['candidates'])
        self.assertEqual(result['earliest_observed_evidence']['rx_monotonic_us'], 550000)

    def test_ring_shortage_without_callback_delay(self):
        rows = normal()
        callback = next(r for r in rows if r['record_type'] == 'audio_callback' and r['rx_monotonic_us'] == 590000)
        callback.update(missing_input_samples=2, starvation_frames=100, ring_before=1, ring_after=0)
        result = classify(rows, event('AUDIO_CALLBACK_STARVATION'))
        self.assertIn('Playback Ring Starvation', result['candidates'])
        self.assertNotIn('Scheduling Delay', result['candidates'])

    def test_session_recovery_isolation(self):
        rows = normal()
        rows.append(row('event', 300000, event_type='AUDIO_RECOVERY', reason='jitter_packet_delivery_resumed'))
        other = dict(event(), stream_id=2, session_id='new-session')
        output = analysis.analyze(rows+[other])
        result = next(r for r in output if r['record_type'] == 'root_cause_candidates')
        self.assertEqual(result['udp_arrivals'], 0)
        self.assertIn('Unknown / Insufficient Evidence', result['candidates'])
        self.assertEqual(result['resync_or_recovery'], [])

    def test_disabled_dropped_and_headless_are_unknown(self):
        for fault in ('disabled', 'drops', 'headless', 'health_gap'):
            rows = normal()
            if fault == 'headless':
                rows = [r for r in rows if r['record_type'] != 'audio_callback']
            elif fault == 'health_gap':
                rows = [r for r in rows if r['record_type'] != 'diagnostic_health' or r['rx_monotonic_us'] not in (200000, 300000, 400000)]
            else:
                h = next(r for r in rows if r['record_type'] == 'diagnostic_health' and r['rx_monotonic_us'] == 300000)
                h['enabled' if fault == 'disabled' else 'observation_drops'] = False if fault == 'disabled' else 1
            result = classify(rows)
            self.assertIn('Unknown / Insufficient Evidence', result['candidates'])

    def test_capture_window_retains_preceding_evidence(self):
        rows = normal()+[event()]
        meta = dict(clock_id='clock', session_id='session', stream_id=1, started_monotonic_us=550000, ended_monotonic_us=600000)
        result = analysis.analyze(rows, meta)
        self.assertEqual(result[0]['packets']['count'], 6)
        self.assertEqual(result[-1]['udp_arrivals'], 51)

    def test_empty_population_and_percentiles(self):
        self.assertEqual(analysis.stats([1, 2, 3, 4], 'test')['p95'], 4)
        empty = analysis.stats([], 'test')
        self.assertFalse(empty['collected'])
        self.assertIsNone(empty['p99'])

    def test_short_disable_between_health_snapshots_is_unknown(self):
        rows = normal()
        for r in rows:
            if r['record_type'] == 'diagnostic_health':
                r['enabled_changes'] = 2 if r['rx_monotonic_us'] >= 400000 else 0
        self.assertIn('Unknown / Insufficient Evidence', classify(rows)['candidates'])

    def test_pending_reorder_hole_and_delayed_followup_are_not_final_loss(self):
        rows = normal()
        for r in rows:
            if r['record_type'] == 'udp_receive':
                r['pending_sequence_holes'] = 1 if r['rx_monotonic_us'] == 590000 else 0
        rows.append(row('udp_receive', 610000, packet_sequence=60, interval_us=10000, sequence_gaps=0))
        result = classify(rows)
        self.assertEqual(result['pending_sequence_holes_max'], 1)
        self.assertFalse(result['udp_normal_observed'])
        self.assertNotIn('Network Starvation', result['candidates'])
        self.assertEqual(len(result['expected_sequence_follow_up_500ms']), 1)

    def test_jitter_residence_population(self):
        rows = [row('jitter_enqueue', 100, packet_sequence=0, admitted=True, capacity=8),
                row('jitter_enqueue', 200, packet_sequence=1, admitted=False, capacity=8),
                row('jitter_dequeue', 300, expected_sequence=0, delivered=True, occupancy=0, capacity=8)]
        summary = analysis.analyze(rows)[0]
        self.assertEqual(summary['jitter_residence_us']['count'], 1)
        self.assertEqual(summary['jitter_residence_us']['p50'], 200)
        self.assertEqual(summary['jitter_capacity_packets'], [8])


if __name__ == '__main__':
    unittest.main()
