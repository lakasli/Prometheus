"""One bounded schedule decision, JSON stdin -> JSON stdout.

The Native host invokes this fixed script after a one-minute notice. This
script has no filesystem, network, recipient, credential or policy capability.
Only the host can commit its normalized result against the claimed event fence.
"""
import json
import sys


def normalize(value):
    if not isinstance(value, dict) or set(value) - {'decision', 'delay_minutes'}:
        raise ValueError('unknown decision fields')
    decision = value.get('decision')
    delay = value.get('delay_minutes', 0)
    if decision not in ('execute', 'cancel', 'postpone') or type(delay) is not int:
        raise ValueError('invalid decision')
    if (decision == 'postpone' and not 1 <= delay <= 30) or (decision != 'postpone' and delay != 0):
        raise ValueError('invalid delay')
    return {'decision': decision, 'delay_minutes': delay}


if __name__ == '__main__':
    try:
        raw = sys.stdin.buffer.read(4097)
        if len(raw) > 4096:
            raise ValueError('input bound')
        result = normalize(json.loads(raw))
        print(json.dumps(result, separators=(',', ':')))
    except (ValueError, TypeError):
        print('schedule_decision_rejected', file=sys.stderr)
        sys.exit(2)
