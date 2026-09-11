-- 模型配置变更后，旧版本向量不能继续作为缓存或参与向量检索。
-- 既有切片 metadata 未记录版本，按初始版本 0 兼容。
ALTER TABLE kb_knowledge_bases ADD COLUMN embedding_revision INTEGER NOT NULL DEFAULT 0;
