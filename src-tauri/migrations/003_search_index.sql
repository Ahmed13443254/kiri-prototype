create virtual table if not exists blocks_fts using fts5(
  text,
  content='blocks',
  content_rowid='id',
  tokenize='unicode61'
);

create trigger if not exists blocks_fts_ai after insert on blocks begin
  insert into blocks_fts(rowid, text) values (new.id, new.text);
end;

create trigger if not exists blocks_fts_ad after delete on blocks begin
  insert into blocks_fts(blocks_fts, rowid, text) values ('delete', old.id, old.text);
end;

create trigger if not exists blocks_fts_au after update on blocks begin
  update blocks_fts set text = new.text where rowid = new.id;
end;
