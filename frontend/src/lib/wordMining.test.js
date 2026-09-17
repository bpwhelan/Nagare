import test from 'node:test';
import assert from 'node:assert/strict';
import { rankedCandidates, makeMiningDraft, validMiningDraft, draftPayload, sentenceParts, unfamiliarCount, miningRequestId } from './wordMining.js';

const cat = { term:'猫', reading:'ねこ', common:true, definitions:[{meaning:'cat'}], part_of_speech:'noun', surfaces:['猫'], occurrences:[0,1] };
const dog = { ...cat, term:'犬', reading:'いぬ', surfaces:['犬'], occurrences:[0] };
const workspace = { history:{duration_ms:20000}, track:{lines:[
  {start_ms:1000,end_ms:4000,text:'猫と犬を見ました。'}, {start_ms:7000,end_ms:9000,text:'猫が寝ています。'}
]}, candidates:[cat,dog], line_words:[['猫','犬','見る'],['猫','寝る']] };

test('recommendations choose the sentence with less unfamiliar vocabulary and preserve its exact timing', () => {
  const known = { 寝る:'known', 見る:'in_anki' };
  const ranked = rankedCandidates(workspace,known,{status:'new',iPlusOne:true});
  assert.deepEqual(ranked.map(c=>c.term),['猫']);
  const draft=makeMiningDraft(cat,workspace,known);
  assert.equal(draft.first,1); assert.equal(draft.sentence,'猫が寝ています。');
  assert.equal(draftPayload(draft).start_ms,6900);
  assert.equal(draftPayload(draft).end_ms,9200);
});
test('known and ignored words leave recommendations; ignoring is not treated as knowing a word', () => {
  assert.deepEqual(rankedCandidates(workspace,{猫:'known',犬:'ignored'},{status:'new'}),[]);
  assert.equal(unfamiliarCount(['犬','犬'],{犬:'ignored'}),1);
  assert.equal(rankedCandidates(workspace,{}, {search:'いぬ'})[0].term,'犬');
});
test('drafts reject invalid clip bounds and preserve milliseconds', () => {
  const draft=makeMiningDraft(cat,workspace);
  assert.ok(validMiningDraft(draft,20000));
  for (const change of [{start:-1},{end:NaN},{end:0},{end:100},{sentence:' '}]) assert.ok(!validMiningDraft({...draft,...change},20000));
  assert.equal(draftPayload({...draft,start:1.234}).start_ms,1234);
});
test('highlighting treats subtitle markup and regex characters as plain text', () => {
  const parts=sentenceParts('<img> 猫 [猫]',{term:'[猫]',surfaces:['猫']});
  assert.deepEqual(parts,[{text:'<img> ',match:false},{text:'猫',match:true},{text:' ',match:false},{text:'[猫]',match:true}]);
});
test('batch IDs work on HTTP LAN origins without crypto.randomUUID', () => {
  const id=miningRequestId();
  assert.match(id,/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  assert.notEqual(id,miningRequestId());
});
