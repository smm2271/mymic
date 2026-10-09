"""Assemble measured artifacts without inventing unavailable focus/listening results."""
import json
from pathlib import Path
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[1]
sources = [
    'before-v051/raw-loopback-result.json',
    'timing-current.json', 'timing-absolute.json', 'timing-period.json',
    'timing-waitable.json', 'timing-waitable-inherited.json',
    'timing-current-inherited.json', 'timing-waitable-mmcss.json',
    'background-audio-600s-before-mmcss.json', 'background-audio-600s.json',
    'timing-loss5.json', 'loopback-analysis.json',
]
measurements = {}
for source in sources:
    path = ROOT / 'artifacts' / source
    measurements[source] = json.loads(path.read_text(encoding='utf-8')) if path.exists() else {'status': 'not measured'}
result = {
    'generated_at_utc': datetime.now(timezone.utc).isoformat(),
    'version': '0.5.2',
    'user_reported_reproduction': {
        'foreground': 'near 0% displayed loss', 'background': 'approximately 25% displayed loss',
        'source': 'user observations, not agent measurements',
    },
    'confirmed_code_defect': 'Skipped Tokio ticks advanced neither sample timeline nor sequence: slower source can create playout deficits without UDP gaps.',
    'os_visibility_root_cause': 'not conclusively established; native focus/minimization unavailable',
    'fix': 'Dedicated absolute-deadline TX, high-resolution waitable timer, RAII MMCSS Audio registration, bounded skip policy, separate diagnostics.',
    'excluded_claims': ['RX sequence gap does not prove UDP network loss', 'period-1 does not guarantee 1 ms wakes', 'loopback analysis is not human listening'],
    'hypotheses': {
        'stdout_blocking': 'User redirection did not fix; new logging is off TX thread. Not conclusively excluded in original binary.',
        'timer_and_scheduling': 'Measured improvement with waitable plus MMCSS; no kernel trace identifying visibility-specific mechanism.',
        'network_loss': 'Raw normal comparison has equal successful TX sends and authenticated RX receipts; no evidence of UDP loss in that run.',
    },
    'acceptance_matrix': [
        {'scene': 'foreground', 'seconds': 60, 'status': 'Pending Manual Verification'},
        {'scene': 'background visible window', 'seconds': 60, 'status': 'Pending Manual Verification'},
        {'scene': 'minimized', 'seconds': 60, 'status': 'Pending Manual Verification'},
        {'scene': 'hidden background redirected', 'seconds': 60, 'source': 'timing-waitable-mmcss.json'},
        {'scene': 'foreground/background switching', 'seconds': 120, 'status': 'Pending Manual Verification'},
        {'scene': 'hidden background long run', 'seconds': 600, 'source': 'background-audio-600s.json', 'limitation': 'Occasional callback starvation remains; audible continuity not fully accepted'},
    ],
    'manual_listening': 'Pending Manual Verification',
    'comparison_limitations': 'Not randomized or fully load-isolated. Current policy runs use new isolated thread; original binary is identified separately.',
    'measurements': measurements,
}
output = ROOT / 'artifacts/bug-001-results.json'
output.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
print(output)
