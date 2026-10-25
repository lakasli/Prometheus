#!/usr/bin/env python3
"""Offline validation of design artifacts. Does not run a task scheduler.

Requires jsonschema with Draft 2020-12 support. Run from any directory:
python tools/validate-task-trigger-design.py --report <optional-json>
"""
import argparse
import copy
import datetime
import json
import pathlib

from jsonschema import Draft202012Validator, FormatChecker


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report')
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[1]
    schema = json.loads((root / 'docs/schemas/dsh-task-trigger-v1.schema.json').read_text())
    Draft202012Validator.check_schema(schema)
    formats = FormatChecker()

    # jsonschema's optional RFC3339 extra is not assumed installed. Require an
    # aware, real Gregorian instant in addition to the schema's strict pattern.
    @formats.checks('date-time', raises=(ValueError, TypeError))
    def instant(value):
        if not isinstance(value, str):
            return True  # The schema's type check supplies the error.
        return datetime.datetime.fromisoformat(value.replace('Z', '+00:00')).tzinfo is not None

    validator = Draft202012Validator(schema, format_checker=formats)
    examples = {p.stem: json.loads(p.read_text(encoding='utf-8'))
                for p in sorted((root / 'docs/examples/task-triggers').glob('*.json'))}
    assert len(examples) == 11
    for value in examples.values():
        validator.validate(value)
    # Paths describe intentional malformed proposals, not runtime implementations.
    cases = [
        ('unknown_enabled', 'once-at', ['enabled'], True),
        ('forged_owner', 'once-at', ['owner_id'], 'forged'),
        ('wrong_spec', 'once-at', ['spec'], 'other'),
        ('empty_title', 'once-at', ['title'], ''),
        ('overlong_title', 'once-at', ['title'], 'x' * 129),
        ('floating_timestamp', 'once-at', ['trigger', 'at'], '2026-11-01T09:00:00'),
        ('invalid_once_date', 'once-at', ['trigger', 'at'], '2026-02-30T09:00:00+08:00'),
        ('delay_zero', 'once-after', ['trigger', 'after_seconds'], 0),
        ('delay_fraction', 'once-after', ['trigger', 'after_seconds'], 1.5),
        ('interval_zero', 'interval', ['trigger', 'every_seconds'], 0),
        ('interval_boolean', 'interval', ['trigger', 'every_seconds'], True),
        ('wrong_anchor', 'interval', ['trigger', 'anchor'], 'every_restart'),
        ('implicit_immediate', 'interval', ['trigger', 'first_fire'], 'now'),
        ('unknown_calendar', 'workday', ['trigger', 'calendar_id'], 'guessed'),
        ('missing_calendar', 'workday', ['trigger', 'calendar_id'], None),
        ('cn_foreign_zone', 'workday', ['time_zone'], 'America/New_York'),
        ('any_with_calendar', 'weekdays', ['trigger', 'calendar_id'], 'cn-official'),
        ('weekday_eight', 'weekdays', ['trigger', 'weekdays'], [8]),
        ('duplicate_weekday', 'weekdays', ['trigger', 'weekdays'], [1, 1]),
        ('month_day_zero', 'month-end', ['trigger', 'month_days'], [0]),
        ('month_thirteen', 'leap-day', ['trigger', 'month_dates'], [{'month': 13, 'day': 1}]),
        ('invalid_date_list', 'date-list', ['trigger', 'dates'], ['2026-02-30']),
        ('duplicate_dates', 'date-list', ['trigger', 'dates'], ['2026-11-01', '2026-11-01']),
        ('leap_second', 'restday', ['trigger', 'at_times'], ['10:00:60']),
        ('duplicate_times', 'restday', ['trigger', 'at_times'], ['10:00:00', '10:00:00']),
        ('event_regex', 'event', ['trigger', 'filters', 0, 'operator'], 'regex'),
        ('event_private_text', 'event', ['trigger', 'filters', 0, 'field'], 'data.text'),
        ('event_code', 'event', ['trigger', 'script'], 'untrusted code'),
        ('arbitrary_delivery', 'once-at', ['delivery'], {'kind': 'webhook', 'url': 'https://example.invalid'}),
        ('secret_field', 'assistant-workday', ['action', 'api_key'], 'synthetic-never-allowed'),
        ('zero_model_budget', 'assistant-workday', ['budget', 'daily_model_runs'], 0),
        ('notify_model_charge', 'once-at', ['budget', 'daily_model_runs'], 1),
        ('coalesce_zero_window', 'once-at', ['policy', 'misfire'], 'coalesce_one'),
        ('native_without_skill', 'assistant-workday', ['action', 'output_mode'], 'native_image'),
        ('context_escape', 'assistant-workday', ['action', 'context_policy'], 'all_private_chats'),
        ('invalid_persona', 'assistant-workday', ['action', 'persona'], {'mode': 'character_revision', 'card_id': 'guessed', 'revision': 1}),
    ]
    rejected = []
    for name, fixture, path, replacement in cases:
        value = copy.deepcopy(examples[fixture])
        target = value['task_trigger_draft']
        for key in path[:-1]:
            target = target[key]
        target[path[-1]] = replacement
        assert list(validator.iter_errors(value)), name
        rejected.append(name)
    for fixture, key in [('once-at', 'at'), ('month-end', 'month_days')]:
        value = copy.deepcopy(examples[fixture])
        del value['task_trigger_draft']['trigger'][key]
        assert list(validator.iter_errors(value)), key
        rejected.append('missing_' + key)
    model = copy.deepcopy(examples['assistant-workday'])
    model['task_trigger_draft']['trigger'] = {'type': 'interval', 'every_seconds': 59,
                                             'anchor': 'first_enable', 'first_fire': 'after_interval'}
    assert list(validator.iter_errors(model))
    rejected.append('model_interval_under_sixty')
    model['task_trigger_draft']['trigger']['every_seconds'] = 60
    validator.validate(model)
    catchup = copy.deepcopy(examples['once-at'])
    catchup['task_trigger_draft']['policy'].update(misfire='bounded_catch_up', catch_up_window_seconds=3600, max_catch_up=3)
    validator.validate(catchup)
    model['task_trigger_draft']['policy'].update(misfire='bounded_catch_up', catch_up_window_seconds=3600, max_catch_up=3)
    assert list(validator.iter_errors(model))
    rejected.append('model_bounded_catchup')
    coalesce = copy.deepcopy(examples['once-at'])
    coalesce['task_trigger_draft']['policy'].update(misfire='coalesce_one', catch_up_window_seconds=3600)
    validator.validate(coalesce)
    coalesce['task_trigger_draft']['policy']['max_catch_up'] = 3
    assert list(validator.iter_errors(coalesce))
    rejected.append('coalesce_three')
    drawing = copy.deepcopy(examples['assistant-workday'])
    drawing['task_trigger_draft']['action'].update(output_mode='html_drawing', skill={'id': 'drawing', 'revision': 1})
    validator.validate(drawing)
    text = (root / 'docs/skills/task-trigger-setup.md').read_text(encoding='utf-8').split('## 指令正文\n\n', 1)[1].strip()
    skill = json.loads((root / 'docs/examples/task-trigger-setup.skill.json').read_text(encoding='utf-8'))
    assert skill['instructions'] == text and skill['enabled'] is False and skill['kind'] == 'instruction'
    assert len(text.encode()) <= 16000 and len(skill['name'].encode()) <= 128 and len(skill['description'].encode()) <= 1024
    assert datetime.date(2026, 10, 10).isoweekday() == 6
    assert datetime.date(2026, 10, 1).isoweekday() == 4
    report = {
        'status': 'passed', 'kind': 'design artifacts static validation only',
        'verified_at': datetime.datetime.now(datetime.timezone(datetime.timedelta(hours=8))).isoformat(),
        'schema_draft': '2020-12', 'schema_examples_passed': len(examples),
        'additional_valid_cases': 4, 'rejection_cases_passed': len(rejected), 'rejection_cases': rejected,
        'skill_instruction_utf8_bytes': len(text.encode()), 'skill_draft_matches_text': True,
        'runtime_implemented': False, 'scheduler_executed': False,
        'remote_deployed': False, 'live_model_called': False,
        'requires_server_semantic_validation': [
            'real_IANA_zone_and_offset', 'calendar_coverage_and_version',
            'source_ACL_catalog_field_type', 'task_and_dependency_revision',
            'UTF8_trim_and_JSON_duplicate_depth_node_limits', 'active_window_and_once_due',
            'catch_up_window_greater_than_grace', 'actual_Skill_kind_and_output_mode',
            'quota_window_and_clock_trust',
        ],
    }
    if args.report:
        pathlib.Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({k: v for k, v in report.items()
                      if k not in ('rejection_cases', 'requires_server_semantic_validation')}, ensure_ascii=False))


if __name__ == '__main__':
    main()
