import { Checkbox, Flex, Select, Text, TextArea, TextField } from '@radix-ui/themes';
import { useId } from 'react';

interface TextInputProps {
  name: string;
  label: string;
  hint?: string;
  required?: boolean;
  defaultValue?: string;
  maxLength?: number;
  multiline?: boolean;
}

interface Option {
  value: string;
  label: string;
}

interface SelectInputProps {
  name: string;
  label: string;
  options: Option[];
  defaultValue: string;
}

interface CheckInputProps {
  name: string;
  label: string;
  defaultChecked?: boolean;
}

const TEXT_LIMIT = 255;

export function TextInput({
  name,
  label,
  hint,
  required = false,
  defaultValue,
  maxLength = TEXT_LIMIT,
  multiline = false,
}: TextInputProps) {
  const id = useId();
  const hintId = `${id}-hint`;
  const described = hint === undefined ? undefined : hintId;
  return (
    <Flex direction="column" gap="1" flexGrow="1">
      <Text as="label" htmlFor={id} size="2" weight="medium">
        {label}
      </Text>
      {multiline ? (
        <TextArea
          id={id}
          name={name}
          required={required}
          defaultValue={defaultValue}
          maxLength={maxLength}
          autoComplete="off"
          spellCheck={false}
          rows={3}
          aria-describedby={described}
        />
      ) : (
        <TextField.Root
          id={id}
          name={name}
          required={required}
          defaultValue={defaultValue}
          maxLength={maxLength}
          autoComplete="off"
          spellCheck={false}
          aria-describedby={described}
        />
      )}
      {hint === undefined ? null : (
        <Text id={hintId} size="1" color="gray">
          {hint}
        </Text>
      )}
    </Flex>
  );
}

export function SelectInput({ name, label, options, defaultValue }: SelectInputProps) {
  const id = useId();
  return (
    <Flex direction="column" gap="1">
      <Text as="label" htmlFor={id} size="2" weight="medium">
        {label}
      </Text>
      <Select.Root name={name} defaultValue={defaultValue}>
        <Select.Trigger id={id} />
        <Select.Content position="popper">
          {options.map((option) => (
            <Select.Item key={option.value} value={option.value}>
              {option.label}
            </Select.Item>
          ))}
        </Select.Content>
      </Select.Root>
    </Flex>
  );
}

export function CheckInput({ name, label, defaultChecked = false }: CheckInputProps) {
  return (
    <Text as="label" size="2">
      <Flex as="span" align="center" gap="2">
        <Checkbox name={name} defaultChecked={defaultChecked} />
        {label}
      </Flex>
    </Text>
  );
}

export function textOf(form: FormData, name: string): string {
  return String(form.get(name) ?? '').trim();
}

export function listOf(form: FormData, name: string): string[] {
  return textOf(form, name)
    .split(/[\s,]+/)
    .filter((item) => item !== '');
}
