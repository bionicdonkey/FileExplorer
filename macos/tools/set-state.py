#!/usr/bin/env python3
"""Sets a feature's macOS state in spec/features.json: set-state.py <feature-id> <state> [note]."""
import json, os, sys
path = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', 'spec', 'features.json')
data = json.load(open(path))
feature = [f for f in data['features'] if f['id'] == sys.argv[1]]
if not feature:
    sys.exit('no such feature: ' + sys.argv[1])
state = {'state': sys.argv[2]}
if len(sys.argv) > 3:
    state['note'] = sys.argv[3]
feature[0]['macos'] = state
json.dump(data, open(path, 'w'), indent=2, ensure_ascii=False)
open(path, 'a').write('\n')
