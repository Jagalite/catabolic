// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
'use strict';
const $ = id => document.getElementById(id);
function element(tag, value, className) {
  const e = document.createElement(tag); e.textContent = value;
  if (className) e.className = className;
  return e;
}
function tree(target, files, previous = []) {
  const root = {children: new Map()};
  const old = new Map(previous.map(file => [file.path, file.target]));
  for (const file of files) {
    let node = root;
    for (const part of file.path.split('/')) {
      if (!node.children.has(part)) node.children.set(part, {children: new Map()});
      node = node.children.get(part);
    }
    node.file = file;
  }
  function branch(parent) {
    const list = document.createElement('ul');
    for (const [name, node] of parent.children) {
      const li = document.createElement('li');
      const row = element('div', '', 'tree-row');
      row.append(element('span', node.file ? '↗' : '▾', node.file ? 'file-icon' : 'folder-icon'), element('span', name, node.file ? 'filename' : 'folder'));
      if (node.file?.target) {
        const changed = previous.length && old.get(node.file.path) !== node.file.target;
        if (changed) row.append(element('span', old.has(node.file.path) ? 'retargeted' : 'added', 'change'));
        row.append(element('span', `→ ${node.file.target.replace('/demo/', '')}`, 'target'));
      }
      li.append(row);
      if (node.children.size) li.append(branch(node));
      list.append(li);
    }
    return list;
  }
  target.replaceChildren(branch(root));
}
let data, selectedFiles = [], queryKey = 'all', layoutKey = 'plex', language = 'sql';
function renderQuery() {
  const q = data.explorer.selections[queryKey];
  $('sql').textContent = language === 'sql' ? q.sql : (q.graphql || q.graphql_note);
  $('sql-tab').setAttribute('aria-pressed', String(language === 'sql'));
  $('graphql-tab').setAttribute('aria-pressed', String(language === 'graphql'));
}
$('sql-tab').onclick = () => { if (data) { language = 'sql'; renderQuery(); } };
$('graphql-tab').onclick = () => { if (data) { language = 'graphql'; renderQuery(); } };
function render() {
  const selection = data.explorer.trees[`${queryKey}-${layoutKey}`];
  const q = data.explorer.selections[queryKey];
  for (const b of $('projections').children) b.setAttribute('aria-pressed', String(b.dataset.key === layoutKey));
  for (const b of $('queries').children) b.setAttribute('aria-pressed', String(b.dataset.key === queryKey));
  $('title').textContent = `${selection.label} / ${q.label}`;
  $('count').textContent = `${selection.files.length} files`;
  renderQuery();
  tree($('output-tree'), selection.files, selectedFiles);
  selectedFiles = selection.files;
}
fetch('demo.json', {cache: 'no-store'}).then(r => { if (!r.ok) throw Error('Unable to load catalog'); return r.json(); }).then(value => {
  data = value;
  const files = data.explorer.queries.files.result;
  const path = files.columns.indexOf('path'), source = files.columns.indexOf('location');
  tree($('source-tree'), files.rows.map(row => ({path: `${row[source]}/${row[path]}`})));
  for (const {id: key, label} of data.explorer.layouts) {
    const b = element('button', label); b.type = 'button'; b.dataset.key = key;
    b.onclick = () => { layoutKey = key; render(); }; $('projections').append(b);
  }
  for (const [key, q] of Object.entries(data.explorer.selections)) {
    const b = element('button', `${q.label} · ${q.result.rows.length}`); b.type = 'button'; b.dataset.key = key;
    b.onclick = () => { queryKey = key; render(); }; $('queries').append(b);
  }
  $('version').textContent = `Catabolic ${data.version}`;
  $('loading').hidden = true; $('view').hidden = false; render();
}).catch(error => { $('loading').textContent = `${error.message}. Reload to retry.`; });
