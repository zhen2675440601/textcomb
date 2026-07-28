export type UserRole = "user" | "super_admin";

export interface User {
  id: string;
  username: string;
  role: UserRole;
}

export interface ProblemDetails {
  type: string;
  title: string;
  status: number;
  code: string;
  detail: string;
  request_id?: string;
}

export type DocumentFormat = "txt" | "docx" | "pdf";

export interface DocumentRecord {
  id: string;
  original_name: string;
  media_type: string;
  document_format: DocumentFormat;
  size_bytes: number;
  char_count: number | null;
  content_available: boolean;
  created_at: string;
}

export type AnalysisStatus =
  | "queued"
  | "extracting"
  | "analyzing"
  | "verifying"
  | "merging"
  | "rendering"
  | "completed"
  | "failed"
  | "cancel_requested"
  | "cancelled"
  | "expired";

export interface Analysis {
  id: string;
  document_id: string;
  model_profile_id: string;
  status: AnalysisStatus;
  stage: string;
  progress: number;
  total_chunks: number;
  completed_chunks: number;
  error_code: string | null;
  error_message: string | null;
  report_id: string | null;
  created_at: string;
  started_at: string | null;
  completed_at: string | null;
}

export interface ModelProfile {
  id: string;
  name: string;
  provider_kind: ProviderKind;
  base_url: string;
  candidate_model: string;
  verifier_model: string;
  max_concurrency: number;
  enabled: boolean;
  is_reference: boolean;
  shared: boolean;
  created_at: string;
}

export type ProviderKind = "openai_responses" | "openai_compatible" | "anthropic";

export type IssueCategory = "typo" | "punctuation" | "grammar" | "paragraph";
export type IssueLevel = "confirmed" | "suspected";
export type FeedbackVerdict = "correct" | "incorrect" | "disputed";
export type GrammarSubtype =
  | "word_order"
  | "collocation"
  | "missing_or_redundant_component"
  | "mixed_structure"
  | "ambiguity"
  | "illogical"
  | "conjunction"
  | "word_misuse";

export interface SourceLocation {
  document_format: DocumentFormat;
  page?: number;
  line_start?: number;
  line_end?: number;
  paragraph_index?: number;
  sentence_index?: number;
  char_start: number;
  char_end: number;
  quote: string;
}

export interface EvidenceReference {
  source_id: string;
  title: string;
  revision: string;
  source_url: string;
}

export interface Issue {
  id: string;
  category: IssueCategory;
  grammar_subtype?: GrammarSubtype;
  level: IssueLevel;
  location: SourceLocation;
  original_text: string;
  reason: string;
  suggestion: string;
  confidence: number;
  evidence_refs: EvidenceReference[];
  feedback?: FeedbackVerdict;
}

export interface Report {
  schema: "textcomb.report.v1";
  report_id: string;
  job_id: string;
  document: {
    original_name: string;
    format: DocumentFormat;
    char_count: number;
  };
  analysis: {
    provider_kind: string;
    candidate_model: string;
    verifier_model: string;
    prompt_version: string;
    analyzer_version: string;
    reference_profile: boolean;
  };
  summary: {
    total: number;
    confirmed: number;
    suspected: number;
    typo: number;
    punctuation: number;
    grammar: number;
    paragraph: number;
  };
  issues: Issue[];
  complete: boolean;
  generated_at: string;
}

export interface AdminUser {
  id: string;
  username: string;
  role: UserRole;
  status: "active" | "disabled";
  created_at: string;
}

export interface SystemStatus {
  database: "ready";
  queued_jobs: number;
  active_jobs: number;
  active_workers: number;
  failed_jobs_last_24h: number;
  retained_reports: number;
  server_time: string;
}
