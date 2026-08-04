/** Search filter labels / tips used by the wiki sidebar. */

export type KindOption = {
  value: string;
  label: string;
  tip: string;
};

export const KIND_OPTIONS: KindOption[] = [
  {
    value: '',
    label: 'All kinds',
    tip: 'Do not filter by kind — search docs, rules, notes, and lore together.',
  },
  {
    value: 'doc',
    label: 'Doc',
    tip: 'Design / reference Markdown (typically under docs/).',
  },
  {
    value: 'rule',
    label: 'Rule',
    tip: 'Cursor / agent rule files (.cursor/rules, directives).',
  },
  {
    value: 'note',
    label: 'Note',
    tip: 'Working notes and knowledge entries (.wordkeep/notes, …).',
  },
  {
    value: 'lore',
    label: 'Lore',
    tip: 'Story / worldbuilding lore Markdown.',
  },
];

export function kindOption(value: string): KindOption {
  return KIND_OPTIONS.find((option) => option.value === value) || KIND_OPTIONS[0]!;
}
