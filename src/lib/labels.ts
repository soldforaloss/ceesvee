// Human-friendly labels and option lists for delimiters and encodings.

export interface Option {
  value: string;
  label: string;
}

export const DELIMITER_OPTIONS: Option[] = [
  { value: ",", label: "Comma  ,  " },
  { value: "\t", label: "Tab  ⇥  " },
  { value: ";", label: "Semicolon  ;  " },
  { value: "|", label: "Pipe  |  " },
];

export const ENCODING_OPTIONS: Option[] = [
  { value: "UTF-8", label: "UTF-8" },
  { value: "UTF-16LE", label: "UTF-16 LE" },
  { value: "UTF-16BE", label: "UTF-16 BE" },
  { value: "windows-1252", label: "Windows-1252 / Latin-1" },
];

export function delimiterLabel(delimiter: string): string {
  switch (delimiter) {
    case ",":
      return "Comma";
    case "\t":
      return "Tab";
    case ";":
      return "Semicolon";
    case "|":
      return "Pipe";
    case " ":
      return "Space";
    default:
      return `“${delimiter}”`;
  }
}

export function encodingLabel(name: string): string {
  return ENCODING_OPTIONS.find((o) => o.value === name)?.label ?? name;
}
