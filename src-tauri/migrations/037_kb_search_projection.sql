-- 关键词检索使用独立投影：不原地改写历史正文、content_hash 或 embedding。
-- 启动后以 Rust 共用分词器分批补建 NULL 投影；新切片在入库时写入投影。
ALTER TABLE kb_chunks ADD COLUMN search_text TEXT;
CREATE INDEX idx_kb_chunks_search_pending ON kb_chunks(id) WHERE search_text IS NULL;

DROP TRIGGER kb_chunks_ai;
DROP TRIGGER kb_chunks_au;

CREATE TRIGGER kb_chunks_ai AFTER INSERT ON kb_chunks BEGIN
    INSERT INTO kb_chunks_fts(chunk_id, content, symbol_name)
    VALUES (NEW.id, COALESCE(NEW.search_text, NEW.content), COALESCE(NEW.symbol_name, ''));
END;

CREATE TRIGGER kb_chunks_au AFTER UPDATE ON kb_chunks BEGIN
    DELETE FROM kb_chunks_fts WHERE chunk_id = OLD.id;
    INSERT INTO kb_chunks_fts(chunk_id, content, symbol_name)
    VALUES (NEW.id,
        CASE WHEN NEW.content IS NOT OLD.content AND NEW.search_text IS OLD.search_text
            THEN NEW.content ELSE COALESCE(NEW.search_text, NEW.content) END,
        COALESCE(NEW.symbol_name, ''));
END;

-- 兼容直接更新正文的写入路径，下一次检索会补建失效投影。
CREATE TRIGGER kb_chunks_search_dirty AFTER UPDATE OF content ON kb_chunks
WHEN NEW.content IS NOT OLD.content AND NEW.search_text IS OLD.search_text
BEGIN
    UPDATE kb_chunks SET search_text = NULL WHERE id = NEW.id;
END;
