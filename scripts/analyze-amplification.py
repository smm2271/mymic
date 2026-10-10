"""Audit actual-code A.5 exports; never substitutes a jitter implementation."""
import argparse
import json
from pathlib import Path


def audit(report):
    investigation=report['investigation']; steps=investigation['timeline']
    arrivals=[s for s in steps if s['operation']=='arrival']
    discarded=[s for s in steps if s['outcome']=='discarded_packet']
    missing=[s for s in steps if s['outcome']=='concealed_zero_slot']
    received_sequences=[s['packet'][0] for s in arrivals]
    if report['scenario']=='baseline':
        assert (report['missing_total'],report['drops_total'])==(11676,11680), 'Stage A fixture mismatch; inspect schedule rather than tune expected values'
    assert len(received_sequences)==len(set(received_sequences)), 'duplicate ingress in exported window'
    assert all(0<=s['after']['occupancy']<=8 for s in steps)
    assert all(s['after']['received']-s['before']['received']==1 for s in arrivals)
    assert all(s['after']['capacity_and_forward_drops']-s['before']['capacity_and_forward_drops']<=1 for s in arrivals)
    by_slot={s['slot']:s for s in missing}
    chains=[]
    for parent in discarded:
        hole=by_slot.get(parent['packet'][0])
        if not hole:
            continue
        child=next((s for s in discarded if s['virtual_ns']>hole['virtual_ns']),None)
        chains.append(dict(discarded_sequence=parent['packet'][0],discard_us=parent['virtual_ns']/1000,
                           missing_same_slot_us=hole['virtual_ns']/1000,
                           occupancy_before_missing=hole['before']['occupancy'],occupancy_after_missing=hole['after']['occupancy'],
                           next_discarded_sequence=child['packet'][0] if child else None,
                           next_discard_us=child['virtual_ns']/1000 if child else None))
    final=investigation.get('final_state')
    conservation=None
    if final:
        conservation=dict(received=final['received'],delivered=final['delivered'],queued=final['occupancy'],
                          dropped=final['capacity_and_forward_drops'],late=final['late'],duplicates=final['duplicates'],
                          received_minus_delivered_queued_dropped=final['received']-final['delivered']-final['occupancy']-final['capacity_and_forward_drops'])
        assert conservation['received_minus_delivered_queued_dropped']==0
        assert final['delivered']+final['missing']==int(report['virtual_seconds']*100)-2
    if investigation['first_drop_ns'] is not None:
        assert investigation['first_drop_ns']-steps[0]['virtual_ns'] >= 10e9
        assert steps[-1]['virtual_ns']-investigation['first_drop_ns'] >= 10e9
    return dict(scenario=report['scenario'],on_off_equal=report['on_off_equal'],conservation=conservation,
                detail_window_start_ns=steps[0]['virtual_ns'] if steps else None,
                detail_window_end_ns=steps[-1]['virtual_ns'] if steps else None,
                window_arrivals=len(arrivals),window_discards=len(discarded),window_missing=len(missing),
                max_drop_one_arrival=investigation['max_drop_in_one_arrival'],cursor_rewinds=investigation['cursor_rewinds'],
                epoch_transitions=investigation['epoch_transitions'],tail_stable=report['tail_stable'],
                last_60s_missing=report['last_60s_missing'],last_60s_drops=report['last_60s_drops'],first_chains=chains[:8],
                evidence_scope='virtual-clock actual Jitter/Renderer; not a Windows/ESP32 diagnosis')


def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('directory',type=Path);parser.add_argument('--output',required=True,type=Path)
    args=parser.parse_args()
    rows=[audit(json.loads((args.directory/f'{name}.json').read_text(encoding='utf8')))
          for name in ('control','baseline','recover_zero','recover_negative','recover_zero_tick_first')]
    args.output.write_text(json.dumps(rows,ensure_ascii=False,indent=2),encoding='utf8')
    print(json.dumps([{k:r[k] for k in ('scenario','tail_stable','last_60s_missing','last_60s_drops','conservation')} for r in rows],indent=2))


if __name__=='__main__':main()
