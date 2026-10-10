import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location('drift', Path(__file__).resolve().parents[1]/'scripts/analyze-drift.py')
drift = importlib.util.module_from_spec(spec); spec.loader.exec_module(drift)


def rows(ppm=0, device_ppm=0, seconds=130, start=0):
    for t in range(seconds):
        def stat(value):
            return dict(count=1, sum=value, min=value, max=value, mean=value)
        point = dict(rx_monotonic_us=t*1_000_000+990000, source_sample_index=(start+round((t+.99)*16000*(1+ppm/1e6))) & 0xffffffff)
        yield dict(record_type='drift_summary', clock_id='test', session_id='one', stream_id=1, playout_epoch=1, output_epoch=1,
                   window_start_us=t*1_000_000, window_end_us=(t+1)*1_000_000, source_last=point, source_envelope=point,
                   invalid_reasons=[], discarded_summary_buckets_total=0, counts={'udp_receive':100},
                   statistics={'audio_callback.requested_frames':stat(48000*(1+device_ppm/1e6)),
                               'audio_callback.output_rate':stat(48000), 'audio_callback.consumed_input_samples':stat(16000),
                               'jitter_dequeue.occupancy':stat(3+ppm/1e6*100*t), 'audio_callback.ring_before':stat(320)})


class DriftTests(unittest.TestCase):
    def test_known_rates_signed_matrix_and_source_wrap(self):
        for ppm in (-1000,-500,-100,-50,0,50,100,500,1000):
            result=list(drift.analyze(rows(ppm,start=0xffff0000)))[-1]
            self.assertTrue(result['valid']); self.assertAlmostEqual(result['source_host_ppm'],ppm,delta=1)
            self.assertAlmostEqual(result['jitter_occupancy_packets_trend']['slope'],ppm/1e6*100)

    def test_independent_device_clock(self):
        result=list(drift.analyze(rows(100,-100)))[-1]
        self.assertAlmostEqual(result['device_host_ppm'],-100)
        self.assertAlmostEqual(result['source_device_ppm'],200.020002,delta=.1)

    def test_missing_seconds_and_faults_are_not_zero_ppm(self):
        for fault in ('loss','missing','overflow','callback_absent'):
            data=list(rows())
            if fault=='loss': data[20]['invalid_reasons']=['sequence_not_steady']
            elif fault=='missing': data.pop(20)
            elif fault=='overflow': data[20]['discarded_summary_buckets_total']=1
            else: data[20]['statistics'].pop('audio_callback.requested_frames')
            result=list(drift.analyze(data))[-1]
            self.assertFalse(result['valid']);
            if fault != 'callback_absent': self.assertIsNone(result['source_host_ppm'])
            else: self.assertIsNotNone(result['source_host_ppm'])

    def test_epoch_change_needs_new_window(self):
        data=list(rows());data[-1]['output_epoch']=2
        result=list(drift.analyze(data))[-1]
        self.assertFalse(result['valid']);self.assertIn('window_not_collected',result['invalid_reasons'])

    def test_thirty_minutes_streaming_windows_are_bounded(self):
        count=0;last=None
        for last in drift.analyze(rows(50,seconds=1800)):count+=1
        self.assertEqual(count,7200); self.assertTrue(last['valid']); self.assertEqual(last['requested_window_seconds'],120)

    def test_delay_slope_is_apparent_not_crystal_proof(self):
        data=list(rows())
        for t,r in enumerate(data):r['source_envelope']['rx_monotonic_us']+=t*100
        result=list(drift.analyze(data))[-1]
        self.assertAlmostEqual(result['source_host_ppm'],-99.99,delta=1)
        self.assertIn('unidentifiable',result['confidence'])


if __name__=='__main__':unittest.main()
