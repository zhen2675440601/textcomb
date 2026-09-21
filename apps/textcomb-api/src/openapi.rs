use crate::state::AppState;
use axum::{Json, Router, routing::get};
use serde_json::{Value, json};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/openapi.json", get(specification))
}

async fn specification() -> Json<Value> {
    Json(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "TextComb API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "文梳中国大陆简体中文辅助校对 API。所有错误使用 application/problem+json。"
        },
        "servers": [{ "url": "/api/v1" }],
        "tags": [
            { "name": "auth" },
            { "name": "documents" },
            { "name": "analyses" },
            { "name": "reports" },
            { "name": "models" },
            { "name": "admin" }
        ],
        "paths": {
            "/auth/login": {
                "post": {
                    "tags": ["auth"],
                    "requestBody": { "$ref": "#/components/requestBodies/Login" },
                    "responses": {
                        "200": {
                            "description": "登录成功",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/User" } } }
                        },
                        "401": { "$ref": "#/components/responses/Problem" }
                    }
                }
            },
            "/auth/logout": {
                "post": {
                    "tags": ["auth"],
                    "responses": { "204": { "description": "已退出" } }
                }
            },
            "/me": {
                "get": {
                    "tags": ["auth"],
                    "responses": {
                        "200": {
                            "description": "当前用户",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/User" } } }
                        }
                    }
                }
            },
            "/documents": {
                "get": {
                    "tags": ["documents"],
                    "responses": {
                        "200": {
                            "description": "文档列表",
                            "content": { "application/json": {
                                "schema": {
                                    "type": "array",
                                    "items": { "$ref": "#/components/schemas/DocumentRecord" }
                                }
                            } }
                        }
                    }
                },
                "post": {
                    "tags": ["documents"],
                    "requestBody": {
                        "required": true,
                        "content": { "multipart/form-data": {
                            "schema": {
                                "type": "object",
                                "required": ["file"],
                                "properties": { "file": { "type": "string", "format": "binary" } }
                            }
                        } }
                    },
                    "responses": {
                        "201": {
                            "description": "上传成功",
                            "content": { "application/json": {
                                "schema": { "$ref": "#/components/schemas/DocumentRecord" }
                            } }
                        },
                        "413": { "$ref": "#/components/responses/Problem" },
                        "422": { "$ref": "#/components/responses/Problem" }
                    }
                }
            },
            "/documents/text": {
                "post": {
                    "tags": ["documents"],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/CreateTextDocument" }
                        } }
                    },
                    "responses": {
                        "201": {
                            "description": "粘贴正文已保存为待分析文档",
                            "content": { "application/json": {
                                "schema": { "$ref": "#/components/schemas/DocumentRecord" }
                            } }
                        },
                        "413": { "$ref": "#/components/responses/Problem" },
                        "422": { "$ref": "#/components/responses/Problem" }
                    }
                }
            },
            "/documents/{id}": {
                "delete": {
                    "tags": ["documents"],
                    "parameters": [{ "$ref": "#/components/parameters/UuidId" }],
                    "responses": { "204": { "description": "已删除" } }
                }
            },
            "/analyses": {
                "get": {
                    "tags": ["analyses"],
                    "responses": {
                        "200": {
                            "description": "任务列表",
                            "content": { "application/json": {
                                "schema": { "type": "array", "items": { "$ref": "#/components/schemas/Analysis" } }
                            } }
                        }
                    }
                },
                "post": {
                    "tags": ["analyses"],
                    "parameters": [{ "$ref": "#/components/parameters/IdempotencyKey" }],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/CreateAnalysis" }
                        } }
                    },
                    "responses": {
                        "202": {
                            "description": "任务已创建并排队",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Analysis" } } }
                        }
                    }
                }
            },
            "/analyses/{id}": {
                "get": {
                    "tags": ["analyses"],
                    "parameters": [{ "$ref": "#/components/parameters/UuidId" }],
                    "responses": {
                        "200": {
                            "description": "任务详情",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Analysis" } } }
                        }
                    }
                },
                "delete": {
                    "tags": ["analyses"],
                    "parameters": [{ "$ref": "#/components/parameters/UuidId" }],
                    "responses": { "204": { "description": "任务和报告已删除" } }
                }
            },
            "/analyses/{id}/events": {
                "get": {
                    "tags": ["analyses"],
                    "parameters": [{ "$ref": "#/components/parameters/UuidId" }],
                    "responses": {
                        "200": {
                            "description": "任务进度事件流",
                            "content": { "text/event-stream": { "schema": { "type": "string" } } }
                        }
                    }
                }
            },
            "/analyses/{id}/cancel": {
                "post": {
                    "tags": ["analyses"],
                    "parameters": [{ "$ref": "#/components/parameters/UuidId" }],
                    "responses": { "202": { "description": "已请求取消" } }
                }
            },
            "/analyses/{id}/retry": {
                "post": {
                    "tags": ["analyses"],
                    "parameters": [
                        { "$ref": "#/components/parameters/UuidId" },
                        { "$ref": "#/components/parameters/IdempotencyKey" }
                    ],
                    "responses": {
                        "202": {
                            "description": "已重新排队",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Analysis" } } }
                        }
                    }
                }
            },
            "/reports/{id}": {
                "get": {
                    "tags": ["reports"],
                    "parameters": [{ "$ref": "#/components/parameters/UuidId" }],
                    "responses": {
                        "200": {
                            "description": "规范化报告",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ReportV1" } } }
                        }
                    }
                },
                "delete": {
                    "tags": ["reports"],
                    "parameters": [{ "$ref": "#/components/parameters/UuidId" }],
                    "responses": { "204": { "description": "已删除" } }
                }
            },
            "/reports/{id}/source": {
                "get": {
                    "tags": ["reports"],
                    "parameters": [
                        { "$ref": "#/components/parameters/UuidId" },
                        { "name": "start", "in": "query", "schema": { "type": "integer", "minimum": 0 } },
                        { "name": "end", "in": "query", "schema": { "type": "integer", "minimum": 0 } }
                    ],
                    "responses": {
                        "200": {
                            "description": "与报告位置对应的提取正文",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ReportSource" } } }
                        },
                        "404": { "$ref": "#/components/responses/Problem" }
                    }
                }
            },
            "/reports/{id}/export/{format}": {
                "get": {
                    "tags": ["reports"],
                    "parameters": [
                        { "$ref": "#/components/parameters/UuidId" },
                        {
                            "name": "format",
                            "in": "path",
                            "required": true,
                            "schema": { "type": "string", "enum": ["json", "md", "pdf"] }
                        }
                    ],
                    "responses": { "200": { "description": "报告文件" } }
                }
            },
            "/reports/{id}/issues": {
                "get": {
                    "tags": ["reports"],
                    "parameters": [
                        { "$ref": "#/components/parameters/UuidId" },
                        { "name": "page", "in": "query", "schema": { "type": "integer", "minimum": 1 } },
                        { "name": "page_size", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 100 } },
                        { "name": "category", "in": "query", "schema": { "type": "string", "enum": ["typo", "punctuation", "grammar", "paragraph"] } },
                        { "name": "level", "in": "query", "schema": { "type": "string", "enum": ["confirmed", "suspected"] } },
                        { "name": "min_confidence", "in": "query", "schema": { "type": "integer", "minimum": 0, "maximum": 100 } },
                        { "name": "feedback", "in": "query", "schema": { "type": "string", "enum": ["correct", "incorrect", "disputed", "unreviewed"] } }
                    ],
                    "responses": { "200": { "description": "筛选和分页后的问题列表" } }
                }
            },
            "/issues/{id}/feedback": {
                "post": {
                    "tags": ["reports"],
                    "parameters": [{ "$ref": "#/components/parameters/UuidId" }],
                    "responses": { "204": { "description": "反馈已保存" } }
                }
            },
            "/model-profiles": {
                "get": {
                    "tags": ["models"],
                    "responses": {
                        "200": {
                            "description": "模型配置列表",
                            "content": { "application/json": {
                                "schema": {
                                    "type": "array",
                                    "items": { "$ref": "#/components/schemas/ModelProfile" }
                                }
                            } }
                        }
                    }
                },
                "post": {
                    "tags": ["models"],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/CreateModelProfile" }
                        } }
                    },
                    "responses": {
                        "201": {
                            "description": "模型配置已创建",
                            "content": { "application/json": {
                                "schema": { "$ref": "#/components/schemas/ModelProfile" }
                            } }
                        }
                    }
                }
            },
            "/model-profiles/{id}/test": {
                "post": {
                    "tags": ["models"],
                    "parameters": [{ "$ref": "#/components/parameters/UuidId" }],
                    "responses": { "200": { "description": "模型连接测试结果" } }
                }
            },
            "/model-profiles/{id}": {
                "patch": {
                    "tags": ["models"],
                    "parameters": [{ "$ref": "#/components/parameters/UuidId" }],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/UpdateModelProfile" }
                        } }
                    },
                    "responses": {
                        "200": {
                            "description": "模型配置已更新",
                            "content": { "application/json": {
                                "schema": { "$ref": "#/components/schemas/ModelProfile" }
                            } }
                        },
                        "404": { "$ref": "#/components/responses/Problem" }
                    }
                }
            },
            "/model-profiles/{id}/enabled": {
                "patch": {
                    "tags": ["models"],
                    "parameters": [{ "$ref": "#/components/parameters/UuidId" }],
                    "responses": { "204": { "description": "模型配置状态已更新" } }
                }
            },
            "/admin/users": {
                "get": {
                    "tags": ["admin"],
                    "responses": { "200": { "description": "用户列表" } }
                },
                "post": {
                    "tags": ["admin"],
                    "responses": { "201": { "description": "用户已创建" } }
                }
            },
            "/admin/system-status": {
                "get": {
                    "tags": ["admin"],
                    "responses": { "200": { "description": "队列、Worker、失败任务与报告保留状态" } }
                }
            },
            "/admin/users/{id}/status": {
                "patch": {
                    "tags": ["admin"],
                    "parameters": [{ "$ref": "#/components/parameters/UuidId" }],
                    "responses": { "204": { "description": "用户状态已更新" } }
                }
            },
            "/admin/users/{id}/password": {
                "post": {
                    "tags": ["admin"],
                    "parameters": [{ "$ref": "#/components/parameters/UuidId" }],
                    "responses": { "204": { "description": "用户密码已重置" } }
                }
            }
        },
        "components": {
            "parameters": {
                "UuidId": {
                    "name": "id",
                    "in": "path",
                    "required": true,
                    "schema": { "type": "string", "format": "uuid" }
                },
                "IdempotencyKey": {
                    "name": "Idempotency-Key",
                    "in": "header",
                    "required": false,
                    "schema": { "type": "string", "minLength": 8, "maxLength": 128 }
                }
            },
            "requestBodies": {
                "Login": {
                    "required": true,
                    "content": { "application/json": {
                        "schema": {
                            "type": "object",
                            "required": ["username", "password"],
                            "properties": {
                                "username": { "type": "string" },
                                "password": { "type": "string", "format": "password" }
                            }
                        }
                    } }
                }
            },
            "responses": {
                "Problem": {
                    "description": "请求失败",
                    "content": { "application/problem+json": {
                        "schema": { "$ref": "#/components/schemas/Problem" }
                    } }
                }
            },
            "schemas": {
                "Problem": {
                    "type": "object",
                    "required": ["type", "title", "status", "code", "detail"],
                    "properties": {
                        "type": { "type": "string", "format": "uri" },
                        "title": { "type": "string" },
                        "status": { "type": "integer" },
                        "code": { "type": "string" },
                        "detail": { "type": "string" },
                        "request_id": { "type": "string" }
                    }
                },
                "User": {
                    "type": "object",
                    "required": ["id", "username", "role"],
                    "properties": {
                        "id": { "type": "string", "format": "uuid" },
                        "username": { "type": "string" },
                        "role": { "type": "string", "enum": ["user", "super_admin"] }
                    }
                },
                "CreateAnalysis": {
                    "type": "object",
                    "required": ["document_id", "model_profile_id"],
                    "properties": {
                        "document_id": { "type": "string", "format": "uuid" },
                        "model_profile_id": { "type": "string", "format": "uuid" },
                        "analysis_profile": { "type": "string", "enum": ["general", "academic", "financial"] }
                    }
                },
                "CreateTextDocument": {
                    "type": "object",
                    "required": ["text"],
                    "properties": {
                        "text": { "type": "string", "minLength": 1, "maxLength": 200000 }
                    }
                },
                "DocumentRecord": {
                    "type": "object",
                    "required": [
                        "id", "original_name", "media_type", "document_format", "size_bytes",
                        "content_available", "created_at"
                    ],
                    "properties": {
                        "id": { "type": "string", "format": "uuid" },
                        "original_name": { "type": "string" },
                        "media_type": { "type": "string" },
                        "document_format": { "type": "string", "enum": ["txt", "docx", "pdf"] },
                        "size_bytes": { "type": "integer", "format": "int64", "minimum": 0 },
                        "char_count": { "type": ["integer", "null"], "minimum": 0 },
                        "content_available": { "type": "boolean" },
                        "created_at": { "type": "string", "format": "date-time" }
                    }
                },
                "Analysis": {
                    "type": "object",
                    "required": [
                        "id", "document_id", "model_profile_id", "analysis_profile", "status", "stage",
                        "progress", "total_chunks", "completed_chunks", "error_code", "error_message",
                        "report_id", "created_at", "started_at", "completed_at"
                    ],
                    "properties": {
                        "id": { "type": "string", "format": "uuid" },
                        "document_id": { "type": "string", "format": "uuid" },
                        "model_profile_id": { "type": "string", "format": "uuid" },
                        "analysis_profile": { "type": "string", "enum": ["general", "academic", "financial"] },
                        "status": { "type": "string" },
                        "stage": { "type": "string" },
                        "progress": { "type": "integer" },
                        "total_chunks": { "type": "integer" },
                        "completed_chunks": { "type": "integer" },
                        "error_code": { "type": ["string", "null"] },
                        "error_message": { "type": ["string", "null"] },
                        "report_id": { "type": ["string", "null"], "format": "uuid" },
                        "created_at": { "type": "string", "format": "date-time" },
                        "started_at": { "type": ["string", "null"], "format": "date-time" },
                        "completed_at": { "type": ["string", "null"], "format": "date-time" }
                    }
                },
                "CreateModelProfile": {
                    "type": "object",
                    "required": [
                        "name", "base_url", "api_key", "candidate_model", "disclosure_accepted"
                    ],
                    "properties": {
                        "name": { "type": "string", "minLength": 1, "maxLength": 100 },
                        "provider_kind": {
                            "type": "string",
                            "enum": ["openai_responses", "openai_compatible", "anthropic"]
                        },
                        "base_url": { "type": "string", "format": "uri", "maxLength": 2048 },
                        "api_key": { "type": "string", "format": "password", "minLength": 1 },
                        "candidate_model": { "type": "string", "minLength": 1, "maxLength": 160 },
                        "verifier_model": { "type": "string", "maxLength": 160 },
                        "max_concurrency": { "type": "integer", "minimum": 1, "maximum": 100 },
                        "shared": { "type": "boolean" },
                        "disclosure_accepted": { "type": "boolean", "const": true }
                    }
                },
                "UpdateModelProfile": {
                    "type": "object",
                    "required": ["name", "base_url", "candidate_model", "disclosure_accepted"],
                    "properties": {
                        "name": { "type": "string", "minLength": 1, "maxLength": 100 },
                        "provider_kind": {
                            "type": "string",
                            "enum": ["openai_responses", "openai_compatible", "anthropic"]
                        },
                        "base_url": { "type": "string", "format": "uri", "maxLength": 2048 },
                        "api_key": {
                            "type": "string",
                            "format": "password",
                            "description": "可选；省略或留空时保留原有密钥"
                        },
                        "candidate_model": { "type": "string", "minLength": 1, "maxLength": 160 },
                        "verifier_model": { "type": "string", "maxLength": 160 },
                        "max_concurrency": { "type": "integer", "minimum": 1, "maximum": 100 },
                        "disclosure_accepted": { "type": "boolean", "const": true }
                    }
                },
                "ModelProfile": {
                    "type": "object",
                    "required": [
                        "id", "name", "provider_kind", "base_url", "candidate_model",
                        "verifier_model", "max_concurrency", "enabled", "is_reference", "shared",
                        "created_at"
                    ],
                    "properties": {
                        "id": { "type": "string", "format": "uuid" },
                        "name": { "type": "string" },
                        "provider_kind": {
                            "type": "string",
                            "enum": ["openai_responses", "openai_compatible", "anthropic"]
                        },
                        "base_url": { "type": "string", "format": "uri" },
                        "candidate_model": { "type": "string" },
                        "verifier_model": { "type": "string" },
                        "max_concurrency": { "type": "integer" },
                        "enabled": { "type": "boolean" },
                        "is_reference": { "type": "boolean" },
                        "shared": { "type": "boolean" },
                        "created_at": { "type": "string", "format": "date-time" }
                    }
                },
                "ReportV1": {
                    "type": "object",
                    "required": [
                        "schema", "report_id", "job_id", "document", "analysis",
                        "summary", "issues", "complete", "generated_at"
                    ],
                    "properties": {
                        "schema": { "const": "textcomb.report.v1" },
                        "report_id": { "type": "string", "format": "uuid" },
                        "job_id": { "type": "string", "format": "uuid" },
                        "document": { "type": "object" },
                        "analysis": {
                            "type": "object",
                            "required": [
                                "analysis_profile", "provider_kind", "candidate_model",
                                "verifier_model", "prompt_version", "analyzer_version", "reference_profile"
                            ],
                            "properties": {
                                "analysis_profile": { "type": "string", "enum": ["general", "academic", "financial"] },
                                "provider_kind": { "type": "string" },
                                "candidate_model": { "type": "string" },
                                "verifier_model": { "type": "string" },
                                "prompt_version": { "type": "string" },
                                "analyzer_version": { "type": "string" },
                                "reference_profile": { "type": "boolean" }
                            }
                        },
                        "summary": { "type": "object" },
                        "issues": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/Issue" }
                        },
                        "complete": { "type": "boolean" },
                        "generated_at": { "type": "string", "format": "date-time" }
                    }
                },
                "ReportSource": {
                    "type": "object",
                    "required": ["original_name", "document_format", "char_count", "char_start", "char_end", "text"],
                    "properties": {
                        "original_name": { "type": "string" },
                        "document_format": { "type": "string", "enum": ["txt", "docx", "pdf"] },
                        "char_count": { "type": "integer", "minimum": 0 },
                        "char_start": { "type": "integer", "minimum": 0 },
                        "char_end": { "type": "integer", "minimum": 0 },
                        "text": { "type": "string" }
                    }
                },
                "Issue": {
                    "type": "object",
                    "required": [
                        "id", "category", "level", "location", "original_text",
                        "reason", "suggestion", "confidence", "evidence_refs"
                    ],
                    "properties": {
                        "id": { "type": "string", "format": "uuid" },
                        "category": {
                            "type": "string",
                            "enum": ["typo", "punctuation", "grammar", "paragraph"]
                        },
                        "grammar_subtype": {
                            "type": ["string", "null"],
                            "enum": [
                                "word_order", "collocation", "missing_component", "redundant_component",
                                "missing_or_redundant_component",
                                "mixed_structure", "ambiguity", "illogical", "conjunction",
                                "word_misuse", null
                            ]
                        },
                        "level": { "type": "string", "enum": ["confirmed", "suspected"] },
                        "location": { "type": "object" },
                        "original_text": { "type": "string" },
                        "reason": { "type": "string" },
                        "suggestion": { "type": "string" },
                        "confidence": { "type": "integer", "minimum": 0, "maximum": 100 },
                        "evidence_refs": { "type": "array", "items": { "type": "object" } },
                        "feedback": {
                            "type": ["string", "null"],
                            "enum": ["correct", "incorrect", "disputed", null]
                        }
                    }
                }
            }
        }
    }))
}
