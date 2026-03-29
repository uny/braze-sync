export type DiffOperation = "add" | "remove" | "change";

export interface DiffDetail {
  field: string;
  operation: DiffOperation;
  localValue?: unknown;
  remoteValue?: unknown;
}

export interface DiffResult {
  resourceType: string;
  resourceName: string;
  operation: DiffOperation;
  details: DiffDetail[];
}

export interface ApplyOptions {
  confirm: boolean;
  allowDestructive: boolean;
}

export interface ApplyResult {
  resourceType: string;
  resourceName: string;
  operation: DiffOperation;
  success: boolean;
  message: string;
}

export interface ValidationError {
  file: string;
  message: string;
}
