"""Three bounded synthetic semantic checks; never read production conversations."""
import argparse
import importlib.util
import json
import pathlib
import re

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-data-dir',required=True)
    parser.add_argument('--source-file',required=True)
    parser.add_argument('--report',required=True)
    args=parser.parse_args()
    source=pathlib.Path(args.source_file).read_text(encoding='utf-8')
    match=re.search(r'"content":"(DSH_SELF_PLAN_COMMIT_V2(?:\\.|[^"\\])*)"',source)
    if not match: raise RuntimeError('committed_semantic_prompt_not_found')
    system=json.loads('"'+match.group(1)+'"')
    spec=importlib.util.spec_from_file_location('probe_relay',pathlib.Path(__file__).with_name('bounded-model-relay.py'))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    relay=module.Relay(args.source_data_dir,3)
    report={'kind':'isolated direct semantic component probe','production_conversations_read':False,'real_channel_tested':False,'status':'running','checks':[]}
    try:
        for reply,expected in [('我趁这会儿去把《鹿柴》抄一抄，抄完也歇。',True),('你说过“我趁这会儿先去整理想法”，我记得。',False),('如果我现在有空，也许会去整理一些东西。',False)]:
            payload={'model':relay.model,'stream':False,'max_tokens':4096,'messages':[{'role':'system','content':system},{'role':'user','content':json.dumps({'reply':reply,'slots':[{'slot_id':'synthetic-slot','start_at':1793090000,'end_at':1793090600}],'capabilities':['save_text_artifact']},ensure_ascii=False)}]}
            status,body=relay.forward(payload)
            if status!=200: raise RuntimeError('provider_http_failed')
            content=json.loads(body)['choices'][0]['message']['content'].strip()
            if content.startswith('```json\n'):content=content[8:].removesuffix('```').strip()
            decision=json.loads(content)
            passed=bool(decision.get('self_activity'))==expected and not decision.get('unfulfilled',False)
            report['checks'].append({'expected_activity':expected,'passed':passed})
            if not passed: raise AssertionError('semantic_component_mismatch')
        report['status']='passed'
    finally:
        report['requests']=relay.records
        pathlib.Path(args.report).write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding='utf-8')
    print(json.dumps({'status':report['status'],'passed':len(report['checks'])}))

if __name__=='__main__':main()
