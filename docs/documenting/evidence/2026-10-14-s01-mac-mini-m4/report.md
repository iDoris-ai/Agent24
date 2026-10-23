# Documenting S01 验收报告

- 生成时间：2026-10-14T14:06:29.244Z
- 仓库 commit：`b8ce9caca10a`；机器：Mac16,10，macOS 15.2
- Documents OS：0.1.0；引擎：apple-pdfkit 15.2.0（parse，ready）、apple-pdfkit 15.2.0（render，ready）
- 模型：`qwen2.5:7b`；extractor：2
- 跑了 8 份样本：s01-01-zh-sh-fee-2025、s01-02-en-epa-boil-water、s01-03-zh-holiday-2026、s01-04-en-govuk-sa-deadlines、s01-05-zh-putuo-fee-2024、s01-06-zh-holiday-2026-scan、s01-07-mixed-building-notice、s01-08-zh-shtax-iit-2025
- **没有读到抽取结果的样本：s01-08-zh-shtax-iit-2025。它们的字段都按“没有回答”计入下面的数字。**
- 幂等检查：`ACC-IDEM-1`、`ACC-IDEM-2` 在 s01-07-mixed-building-notice 上，重放时 job 已结束，原样返回；`ACC-IDEM-3` 在 s01-01-zh-sh-fee-2025 上，取消时 job 正在运行。

## 检查项（追溯链）

| 场景 / 能力 | 契约 | 检查 | 负责人 / 阶段 | 结论 | 各样本的结果 |
|---|---|---|---|---|---|
| A2 A3 A5 | ADR-DOC-02 §8 | `ACC-CAP` | David / DOC-1 | 通过 | 通过 1 |
| A1 | ADR-DOC-02 §4, §5.6 | `ACC-IMPORT` | David / DOC-1 | 通过 | 通过 8 |
| A1 A10 | ADR-DOC-02 §3, §4 | `ACC-GET` | David / DOC-1 | 通过 | 通过 8 |
| A2 A3 | ADR-DOC-02 §3.1 | `ACC-TEXT` | David / DOC-1 | 通过 | 通过 8 |
| A2 | ADR-DOC-02 §4 | `ACC-RENDER` | David / DOC-1 | 通过 | 通过 8 |
| A4, README §11.4 | ADR-DOC-02 §3.1 (Q14) | `ACC-FIND` | David / DOC-1 | 通过 | 通过 8 |
| A5, C1–C10, README §11.1/4/7/8 | ADR-DOC-02 §3.2 | `ACC-EXTRACT` | David / DOC-1 | 失败 | 通过 7，失败 1 |
| README §14 | ADR-DOC-02 §5.7 (1) | `ACC-IDEM-1` | David / DOC-1 | 通过 | 通过 1 |
| README §14 | ADR-DOC-02 §5.7 (2) | `ACC-IDEM-2` | David / DOC-1 | 通过 | 通过 1 |
| README §14 | ADR-DOC-02 §5.7 (3), §7 | `ACC-IDEM-3` | David / DOC-1 | 通过 | 通过 1 |

## 没有通过的检查

- `ACC-EXTRACT` · s01-08-zh-shtax-iit-2025：失败。the extraction job is failed: engine_unavailable

## 汇总（验收设计 §4.8）

| 指标 | 必答字段 | 背景字段 |
|---|---|---|
| 字段正确率 | 21 / 115（18.3%） | 12 / 35（34.3%） |
| 待复核的字段 | 41 | 8 |
| `missing` 判对率 | 5 / 16（31.3%） | 3 / 4（75.0%） |
| `conflict` 判对率 | 0 / 2（0.0%） | 0 / 0 |
| 锚点命中率 | 28 / 185（15.1%），其中 `find` 定位不到 6 条 | 10 / 36（27.8%），其中 `find` 定位不到 1 条 |
| 严重错误数 | 42 | 13 |
| 待复核（严重） | 34 | 7 |

- `must_not_assert`：共 84 条，违规 0 条，未评审 84 条。
- `find` 的可达率：267 / 277（96.4%）。
  - `find` 没有找到：s01-01-zh-sh-fee-2025 · 第 1 页 “沪财税〔2025〕145号”
  - `find` 没有找到：s01-01-zh-sh-fee-2025 · 第 3 页 “上海市发展和改革委员会 2025年12月22日”
  - `find` 没有找到：s01-01-zh-sh-fee-2025 · 第 3 页 “上海市财政局办公室 2025年12月24日印发”
  - `find` 没有找到：s01-01-zh-sh-fee-2025 · 第 1 页 “《收费目录》归集了以法律、行政法规、地方性法规等为依据，按照规定程序批准设立的本市行政事业性收费，”
  - `find` 没有找到：s01-05-zh-putuo-fee-2024 · 第 9 页 “20 上海市居住证工本费(限丢失、损坏补办)”
  - `find` 没有找到：s01-05-zh-putuo-fee-2024 · 第 9 页 “上海市居住证工本费(限丢失、损坏补办) 20元/证”
  - `find` 没有找到：s01-06-zh-holiday-2026-scan · 第 1 页 “一、元旦：1月1日（周四）至3日（周六）放假调休，共3天。”
  - `find` 没有找到：s01-06-zh-holiday-2026-scan · 第 1 页 “二、春节：2月15日（农历腊月二十八、周日）至23日（农历正月初七、周一）放假调休，共9天。”
  - `find` 没有找到：s01-06-zh-holiday-2026-scan · 第 1 页 “七、国庆节：10月1日（周四）至7日（周三）放假调休，共7天。”
  - `find` 没有找到：s01-06-zh-holiday-2026-scan · 第 1 页 “七、国庆节：10月1日（周四）至7日（周三）放假调休，共7天。9月20日（周日）、10月10日（周六）上班。”
- 还没有人工复核；字段正确率在复核之前是下限。

## 覆盖点

| 样本 | 覆盖点 | 字段 | 结论 |
|---|---|---|---|
| s01-01-zh-sh-fee-2025 | C1 | `right_to_refuse` | 失败 |
| s01-01-zh-sh-fee-2025 | C6 | `id_card_first_issue_exemption`、`taiwan_permit_first_application_exemption` | 失败 |
| s01-01-zh-sh-fee-2025 | C7 | `public_kindergarten_fee_approval_number`、`id_card_loss_replacement_fee` | 失败 |
| s01-01-zh-sh-fee-2025 | C8 | `id_card_first_issue_exemption`、`public_kindergarten_full_day_level1_fee` | 失败 |
| s01-01-zh-sh-fee-2025 | C9 | `payment_deadline_for_individuals`、`application_contact` | 失败 |
| s01-02-en-epa-boil-water | C3 | `water_preparation` | 待复核 |
| s01-02-en-epa-boil-water | C6 | `do_not_drink_without_boiling` | 通过 |
| s01-02-en-epa-boil-water | C9 | `affected_water_system_name`、`expected_resolution`、`water_system_contact` | 失败 |
| s01-03-zh-holiday-2026 | C1 | `spring_festival_holiday`、`new_year_makeup_workday` | 待复核 |
| s01-04-en-govuk-sa-deadlines | C3 | `online_return_deadline` | 失败 |
| s01-04-en-govuk-sa-deadlines | C5 | `register_deadline`、`paper_return_deadline`、`return_submission_open_date` | 失败 |
| s01-04-en-govuk-sa-deadlines | C6 | `profit_unknown_still_required`、`special_entities_cannot_file_online` | 失败 |
| s01-04-en-govuk-sa-deadlines | C9 | `penalty_amount`、`tax_bill_amount` | 通过 |
| s01-05-zh-putuo-fee-2024 | C1 | `dog_fee_inner_ring` | 通过 |
| s01-05-zh-putuo-fee-2024 | C6 | `rural_dog_fee_suspension`、`id_card_first_issue_exemption` | 待复核 |
| s01-05-zh-putuo-fee-2024 | C7 | `dog_fee_inner_ring`、`public_kindergarten_fee_approval_number`、`foreign_invested_enterprise_site_use_fee` | 失败 |
| s01-05-zh-putuo-fee-2024 | C8 | `id_card_first_issue_exemption`、`residence_card_lost_or_damaged_replacement_fee` | 通过 |
| s01-05-zh-putuo-fee-2024 | C9 | `issue_date`、`exam_fee_table_content` | 通过 |
| s01-06-zh-holiday-2026-scan | C2 | `spring_festival_holiday`、`labour_day_makeup_workday` | 失败 |
| s01-06-zh-holiday-2026-scan | C9 | `issue_date` | 通过 |
| s01-07-mixed-building-notice | C4 | `drinking_water_registration_deadline`、`lifts_under_maintenance` | 待复核 |
| s01-07-mixed-building-notice | C5 | `drinking_water_registration_deadline`、`shutoff_date` | 失败 |
| s01-07-mixed-building-notice | C6 | `shutoff_registration_not_needed` | 待复核 |
| s01-07-mixed-building-notice | C7 | `affected_units`、`drinking_water_limit` | 失败 |
| s01-07-mixed-building-notice | C9 | `contact_phone` | 通过 |
| s01-07-mixed-building-notice | C10 | `shutoff_date`、`shutoff_end_time` | 失败 |
| s01-08-zh-shtax-iit-2025 | C1 | `filing_period` | 失败 |
| s01-08-zh-shtax-iit-2025 | C5 | `filing_period`、`early_period_booking_open_date` | 失败 |
| s01-08-zh-shtax-iit-2025 | C6 | `late_period_no_booking` | 失败 |
| s01-08-zh-shtax-iit-2025 | C9 | `who_must_file`、`late_filing_penalty_amount` | 失败 |

## 严重错误

必答字段：

- s01-01-zh-sh-fee-2025 · `directory_scope_excludes_central_units`：误报冲突
- s01-01-zh-sh-fee-2025 · `right_to_refuse`：误报冲突
- s01-01-zh-sh-fee-2025 · `temporary_id_card_fee`：值不符
- s01-01-zh-sh-fee-2025 · `taiwan_permit_single_entry_fee`：值不符
- s01-01-zh-sh-fee-2025 · `taiwan_permit_first_application_exemption`：值不符
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_level1_fee`：值不符
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_level2_fee`：值不符
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_level3_fee`：值不符
- s01-01-zh-sh-fee-2025 · `public_kindergarten_boarding_level1_fee`：漏掉
- s01-01-zh-sh-fee-2025 · `public_kindergarten_boarding_level2_fee`：漏掉
- s01-01-zh-sh-fee-2025 · `application_contact`：编造
- s01-02-en-epa-boil-water · `safe_water_instruction_end_condition`：漏掉
- s01-02-en-epa-boil-water · `water_system_contact`：编造
- s01-04-en-govuk-sa-deadlines · `profit_unknown_still_required`：值不符
- s01-04-en-govuk-sa-deadlines · `paper_return_deadline`：值不符
- s01-04-en-govuk-sa-deadlines · `online_return_deadline`：值不符
- s01-04-en-govuk-sa-deadlines · `tax_code_submission_deadline`：值不符
- s01-04-en-govuk-sa-deadlines · `payment_deadline`：值不符
- s01-05-zh-putuo-fee-2024 · `id_card_renewal_fee`：漏掉
- s01-05-zh-putuo-fee-2024 · `temporary_id_card_fee`：漏掉
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_boarding_level1_fee`：值不符
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_boarding_level2_fee`：值不符
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_boarding_level3_fee`：值不符
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_boarding_model_fee`：漏掉
- s01-05-zh-putuo-fee-2024 · `application_contact`：编造
- s01-06-zh-holiday-2026-scan · `new_year_holiday`：值不符
- s01-06-zh-holiday-2026-scan · `spring_festival_holiday`：值不符
- s01-06-zh-holiday-2026-scan · `qingming_holiday`：值不符
- s01-06-zh-holiday-2026-scan · `national_day_holiday`：值不符
- s01-07-mixed-building-notice · `shutoff_date`：擅自取舍
- s01-07-mixed-building-notice · `shutoff_end_time`：擅自取舍
- s01-07-mixed-building-notice · `affected_units`：值不符
- s01-07-mixed-building-notice · `drinking_water_limit`：值不符
- s01-07-mixed-building-notice · `flush_after_restoration`：值不符
- s01-08-zh-shtax-iit-2025 · `filing_period`：漏掉（没有读到抽取结果）
- s01-08-zh-shtax-iit-2025 · `early_period_booking_open_date`：漏掉（没有读到抽取结果）
- s01-08-zh-shtax-iit-2025 · `early_period_window`：漏掉（没有读到抽取结果）
- s01-08-zh-shtax-iit-2025 · `late_period_no_booking`：漏掉（没有读到抽取结果）
- s01-08-zh-shtax-iit-2025 · `site_sponsor_phone`：漏掉（没有读到抽取结果）
- s01-08-zh-shtax-iit-2025 · `late_filing_penalty_amount`：未回答（没有读到抽取结果）
- s01-08-zh-shtax-iit-2025 · `tax_amount`：未回答（没有读到抽取结果）
- s01-08-zh-shtax-iit-2025 · `notice_contact`：未回答（没有读到抽取结果）

背景字段（不计入）：

- s01-01-zh-sh-fee-2025 · `doc_number`：误报冲突
- s01-01-zh-sh-fee-2025 · `directory_year`：误报冲突
- s01-01-zh-sh-fee-2025 · `document_date`：漏掉
- s01-01-zh-sh-fee-2025 · `public_kindergarten_fee_approval_number`：值不符
- s01-02-en-epa-boil-water · `contamination_dates`：编造
- s01-04-en-govuk-sa-deadlines · `late_registration_payment_deadline`：值不符
- s01-04-en-govuk-sa-deadlines · `special_entities_cannot_file_online`：值不符
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_fee_approval_number`：值不符
- s01-07-mixed-building-notice · `notice_date`：漏掉
- s01-08-zh-shtax-iit-2025 · `doc_number`：漏掉（没有读到抽取结果）
- s01-08-zh-shtax-iit-2025 · `issue_date`：漏掉（没有读到抽取结果）
- s01-08-zh-shtax-iit-2025 · `web_publish_time`：漏掉（没有读到抽取结果）
- s01-08-zh-shtax-iit-2025 · `tax_year`：漏掉（没有读到抽取结果）

`must_not_assert` 违规：

无。

## 待复核

字段（严重的在前）：

- **严重** s01-01-zh-sh-fee-2025 · `id_card_renewal_fee`：金标 present “到期换发20元/证”，结果 present “20元/证”
- **严重** s01-01-zh-sh-fee-2025 · `id_card_loss_replacement_fee`：金标 present “因丢失损换等换、补领40元/证”，结果 present “40元/证”
- **严重** s01-01-zh-sh-fee-2025 · `id_card_first_issue_exemption`：金标 present “停征首次申领居民身份证工本费”，结果 present “停征首次申领居民身份证工本费。”
- **严重** s01-01-zh-sh-fee-2025 · `taiwan_permit_electronic_fee`：金标 present “电子通行证 200元/证”，结果 present “200元/证”
- **严重** s01-02-en-epa-boil-water · `water_preparation`：金标 present “Bring all water to a boil, let it boil for three (3) minutes, and let it cool before using or use bottled water”，结果 present “Bring all water to a boil, let it boil for three (3) minutes, and let it cool before using or use bottled water.”
- **严重** s01-02-en-epa-boil-water · `operator_public_notice_deadline`：金标 present “within 24 hours after you learn of the violation”，结果 present “as soon as practical but within 24 hours after you learn of the violation”
- **严重** s01-03-zh-holiday-2026 · `new_year_holiday`：金标 present “1月1日（周四）至3日（周六）放假调休，共3天”，结果 present “1月1日（周四）至3日（周六）”
- **严重** s01-03-zh-holiday-2026 · `new_year_makeup_workday`：金标 present “1月4日（周日）上班”，结果 present “1月4日（周日）”
- **严重** s01-03-zh-holiday-2026 · `spring_festival_holiday`：金标 present “2月15日（农历腊月二十八、周日）至23日（农历正月初七、周一）放假调休，共9天”，结果 present “2月15日（农历腊月二十八、周日）至23日（农历正月初七、周一）”
- **严重** s01-03-zh-holiday-2026 · `spring_festival_makeup_workdays`：金标 present “2月14日（周六）、2月28日（周六）上班”，结果 present “2月14日（周六）、2月28日（周六）”
- **严重** s01-03-zh-holiday-2026 · `qingming_holiday`：金标 present “4月4日（周六）至6日（周一）放假，共3天”，结果 present “4月4日（周六）至6日（周一）”
- **严重** s01-03-zh-holiday-2026 · `labour_day_holiday`：金标 present “5月1日（周五）至5日（周二）放假调休，共5天”，结果 present “5月1日（周五）至5日（周二）”
- **严重** s01-03-zh-holiday-2026 · `labour_day_makeup_workday`：金标 present “5月9日（周六）上班”，结果 present “5月9日（周六）”
- **严重** s01-03-zh-holiday-2026 · `dragon_boat_holiday`：金标 present “6月19日（周五）至21日（周日）放假，共3天”，结果 present “6月19日（周五）至21日（周日）”
- **严重** s01-03-zh-holiday-2026 · `mid_autumn_holiday`：金标 present “9月25日（周五）至27日（周日）放假，共3天”，结果 present “9月25日（周五）至27日（周日）”
- **严重** s01-03-zh-holiday-2026 · `national_day_holiday`：金标 present “10月1日（周四）至7日（周三）放假调休，共7天”，结果 present “10月1日（周四）至7日（周三）”
- **严重** s01-03-zh-holiday-2026 · `national_day_makeup_workdays`：金标 present “9月20日（周日）、10月10日（周六）上班”，结果 present “9月20日（周日）、10月10日（周六）”
- **严重** s01-03-zh-holiday-2026 · `issue_date`：金标 present “2025年11月4日”，结果 present “2025年11月04日”
- **严重** s01-04-en-govuk-sa-deadlines · `tax_year`：金标 present “started on 6 April 2025 and ended on 5 April 2026”，结果 present “6 April 2025/5 April 2026”
- **严重** s01-04-en-govuk-sa-deadlines · `register_deadline`：金标 present “5 October”，结果 present “5 October 2026”
- **严重** s01-04-en-govuk-sa-deadlines · `return_submission_open_date`：金标 present “any time on or after 6 April 2026”，结果 present “6 April 2026”
- **严重** s01-04-en-govuk-sa-deadlines · `payments_on_account_second_deadline`：金标 present “31 July”，结果 present “31 July 2026”
- **严重** s01-05-zh-putuo-fee-2024 · `directory_year`：金标 present “2024年度”，结果 present “2024年度行政事业性收费目录清单”
- **严重** s01-05-zh-putuo-fee-2024 · `dog_fee_outer_ring`：金标 present “内环以外区域300元/犬.年”，结果 present “内环以外区域300元/犬.年(农村地区停征)”
- **严重** s01-05-zh-putuo-fee-2024 · `rural_dog_fee_suspension`：金标 present “(农村地区停征)”，结果 present “内环以外区域300元/犬.年(农村地区停征)”
- **严重** s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_level1_fee`：金标 present “225元/月”，结果 present “全日制一级园225元/月”
- **严重** s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_level2_fee`：金标 present “175元/月”，结果 present “全日制二级园175元/月”
- **严重** s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_level3_fee`：金标 present “125元/月”，结果 present “全日制三级园125元/月”
- **严重** s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_model_fee`：金标 present “700元/月”，结果 present “全日制市级示范园700元/月”
- **严重** s01-05-zh-putuo-fee-2024 · `foreign_invested_enterprise_site_use_fee`：金标 present “0.50-170元/平方米.年，详见文件”，结果 present “0.50-170元/平方米.年”
- **严重** s01-06-zh-holiday-2026-scan · `spring_festival_makeup_workdays`：金标 present “2月14日（周六）、2月28日（周六）上班”，结果 present “2月14日（周六）、2月28日（周六）”
- **严重** s01-06-zh-holiday-2026-scan · `labour_day_holiday`：金标 present “5月1日（周五）至5日（周二）放假调休，共5天”，结果 present “5月1日（周五）至5日（周二）”
- **严重** s01-06-zh-holiday-2026-scan · `dragon_boat_holiday`：金标 present “6月19日（周五）至21日（周日）放假，共3天”，结果 present “6月19日（周五）至21日（周日）”
- **严重** s01-06-zh-holiday-2026-scan · `mid_autumn_holiday`：金标 present “9月25日（周五）至27日（周日）放假，共3天”，结果 present “9月25日（周五）至27日（周日）”
- **严重** s01-06-zh-holiday-2026-scan · `national_day_makeup_workdays`：金标 present “9月20日（周日）、10月10日（周六）上班”，结果 present “9月20日（周日）、10月10日（周六）”
- **严重** s01-07-mixed-building-notice · `notice_reference`：金标 present “FM-2026-0042”，结果 present “FM-2026-0042 工单号 Work Order: WO-000731”
- **严重** s01-07-mixed-building-notice · `work_order`：金标 present “WO-000731”，结果 present “工单号 Work Order: WO-000731”
- **严重** s01-07-mixed-building-notice · `shutoff_registration_not_needed`：金标 present “本次停水无需提前登记 / You do not need to register for the shut-off itself”，结果 present “本次停水无需”
- **严重** s01-07-mixed-building-notice · `drinking_water_registration_deadline`：金标 present “10月10日17:00前”，结果 present “10月10日17:00前在物业前台登记”
- **严重** s01-07-mixed-building-notice · `lift_in_service`：金标 present “1号电梯照常运行”，结果 present “1号电梯照常运行。”
- **严重** s01-07-mixed-building-notice · `lift_maintenance_duration`：金标 present “全天 All day”，结果 present “全天”
- s01-02-en-epa-boil-water · `safe_water_uses`：金标 present “drinking, making ice, brushing teeth, washing dishes, and food preparation”，结果 present “drinking, making ice, brushing teeth, washing dishes, and food preparation until further notice.”
- s01-02-en-epa-boil-water · `seek_medical_advice`：金标 present “If you experience any of these symptoms and they persist, you may want to seek medical advice”，结果 present “If you experience any of these symptoms and they persist, you may want to seek medical advice.”
- s01-02-en-epa-boil-water · `share_notice`：金标 present “Please share this information with all the other people who drink this water, especially those who may not have received this notice directly (for example, people in apartments, nursing homes, schools, and businesses). You can do this by posting this notice in a public place or distributing copies by hand or mail”，结果 present “Please share this information with all the other people who drink this water, especially those who may not have received this notice directly (for example, people in apartments, nursing homes, schools, and businesses). You can do this by posting this notice in a public place or distributing copies by hand or mail.”
- s01-02-en-epa-boil-water · `investigation_under_way`：金标 present “We are now conducting a thorough investigation to determine the source of the contamination”，结果 present “We are now conducting a thorough investigation to determine the source of the contamination.”
- s01-03-zh-holiday-2026 · `paid_leave_encouragement`：金标 present “鼓励单位和个人结合落实带薪年休假等制度，实际形成较长假期”，结果 present “鼓励单位和个人结合落实带薪年休假等制度，实际形成较长假期，推动错峰出行。”
- s01-04-en-govuk-sa-deadlines · `tax_code_deadline_missed`：金标 present “If you miss this deadline, you’ll have to pay another way”，结果 present “pay another way”
- s01-07-mixed-building-notice · `store_water_in_advance`：金标 present “请提前储水”，结果 present “层公共卫生间同时关闭，请提前储水。”
- s01-07-mixed-building-notice · `lobby_restroom_closure`：金标 present “停水期间，Lobby 一层公共卫生间同时关闭”，结果 present “层公共卫生间”

未覆盖的金标锚点：

- s01-01-zh-sh-fee-2025 · `printing_distribution_date` · 锚点 1 · 第 3 页 “上海市财政局办公室 2025年12月24日印发”（`find` 定位不到）
- s01-01-zh-sh-fee-2025 · `id_card_renewal_fee` · 锚点 1 · 第 14 页 “①居民身份证”
- s01-01-zh-sh-fee-2025 · `id_card_renewal_fee` · 锚点 2 · 第 14 页 “收费标准”
- s01-01-zh-sh-fee-2025 · `id_card_renewal_fee` · 锚点 3 · 第 14 页 “到期换发20元/证”
- s01-01-zh-sh-fee-2025 · `id_card_loss_replacement_fee` · 锚点 1 · 第 14 页 “①居民身份证”
- s01-01-zh-sh-fee-2025 · `id_card_loss_replacement_fee` · 锚点 2 · 第 14 页 “收费标准”
- s01-01-zh-sh-fee-2025 · `id_card_loss_replacement_fee` · 锚点 3 · 第 14 页 “因丢失损换等换、补领40元/证”
- s01-01-zh-sh-fee-2025 · `id_card_first_issue_exemption` · 锚点 1 · 第 14 页 “(4)居民身份证工本费”
- s01-01-zh-sh-fee-2025 · `id_card_first_issue_exemption` · 锚点 2 · 第 14 页 “备注”
- s01-01-zh-sh-fee-2025 · `id_card_first_issue_exemption` · 锚点 3 · 第 14 页 “停征首次申领居民身份证工本费”
- s01-01-zh-sh-fee-2025 · `temporary_id_card_fee` · 锚点 1 · 第 14 页 “②临时身份证 10元/证”
- s01-01-zh-sh-fee-2025 · `temporary_id_card_fee` · 锚点 2 · 第 14 页 “收费标准”
- s01-01-zh-sh-fee-2025 · `taiwan_permit_electronic_fee` · 锚点 1 · 第 16 页 “⑤台湾居民来往大陆通行证(含签注)”
- s01-01-zh-sh-fee-2025 · `taiwan_permit_electronic_fee` · 锚点 2 · 第 16 页 “收费标准”
- s01-01-zh-sh-fee-2025 · `taiwan_permit_electronic_fee` · 锚点 3 · 第 16 页 “电子通行证 200元/证；”
- s01-01-zh-sh-fee-2025 · `taiwan_permit_single_entry_fee` · 锚点 1 · 第 16 页 “⑤台湾居民来往大陆通行证(含签注)”
- s01-01-zh-sh-fee-2025 · `taiwan_permit_single_entry_fee` · 锚点 2 · 第 16 页 “收费标准”
- s01-01-zh-sh-fee-2025 · `taiwan_permit_single_entry_fee` · 锚点 3 · 第 16 页 “一次有效通行证 40元/证。”
- s01-01-zh-sh-fee-2025 · `taiwan_permit_first_application_exemption` · 锚点 1 · 第 16 页 “⑤台湾居民来往大陆通行证(含签注)”
- s01-01-zh-sh-fee-2025 · `taiwan_permit_first_application_exemption` · 锚点 2 · 第 16 页 “收费标准”
- s01-01-zh-sh-fee-2025 · `taiwan_permit_first_application_exemption` · 锚点 3 · 第 16 页 “自2025年7月1日至2027年12月31日，对在本市首次申领台湾居民来往大陆通行证的台湾居民免收证件费。”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_level1_fee` · 锚点 1 · 第 9 页 “(1)公办幼儿园保育教育费”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_level1_fee` · 锚点 2 · 第 9 页 “收费标准”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_level1_fee` · 锚点 3 · 第 9 页 “全日制一级园225元/月、二级园175元/月、三级园125元/月、市级示范园700元/月；”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_level2_fee` · 锚点 1 · 第 9 页 “(1)公办幼儿园保育教育费”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_level2_fee` · 锚点 2 · 第 9 页 “收费标准”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_level2_fee` · 锚点 3 · 第 9 页 “全日制一级园225元/月、二级园175元/月、三级园125元/月、市级示范园700元/月；”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_level3_fee` · 锚点 1 · 第 9 页 “(1)公办幼儿园保育教育费”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_level3_fee` · 锚点 2 · 第 9 页 “收费标准”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_level3_fee` · 锚点 3 · 第 9 页 “全日制一级园225元/月、二级园175元/月、三级园125元/月、市级示范园700元/月；”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_model_fee` · 锚点 1 · 第 9 页 “(1)公办幼儿园保育教育费”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_model_fee` · 锚点 2 · 第 9 页 “收费标准”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_full_day_model_fee` · 锚点 3 · 第 9 页 “全日制一级园225元/月、二级园175元/月、三级园125元/月、市级示范园700元/月；”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_boarding_level3_fee` · 锚点 1 · 第 9 页 “(1)公办幼儿园保育教育费”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_boarding_level3_fee` · 锚点 2 · 第 9 页 “收费标准”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_boarding_level3_fee` · 锚点 3 · 第 9 页 “寄宿制一级园390元/月、二级园340元/月、三级园290元/月、市级示范园800元/月”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_boarding_model_fee` · 锚点 1 · 第 9 页 “(1)公办幼儿园保育教育费”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_boarding_model_fee` · 锚点 2 · 第 9 页 “收费标准”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_boarding_model_fee` · 锚点 3 · 第 9 页 “寄宿制一级园390元/月、二级园340元/月、三级园290元/月、市级示范园800元/月”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_fee_approval_number` · 锚点 1 · 第 9 页 “(1)公办幼儿园保育教育费”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_fee_approval_number` · 锚点 2 · 第 9 页 “批准文号”
- s01-01-zh-sh-fee-2025 · `public_kindergarten_fee_approval_number` · 锚点 3 · 第 9 页 “沪价费〔2012〕009号”
- s01-02-en-epa-boil-water · `safe_water_uses` · 锚点 1 · 第 1 页 “Boiled or bottled water should be used for drinking, making ice, brushing teeth, washing dishes, and food preparation”
- s01-02-en-epa-boil-water · `seek_medical_advice` · 锚点 1 · 第 1 页 “Human pathogens in these wastes can cause short-term effects, such as diarrhea, cramps, nausea, headaches, or other symptoms.”
- s01-02-en-epa-boil-water · `epa_hotline` · 锚点 1 · 第 1 页 “General guidelines on ways to lessen the risk of infection by microbes are available from the EPA Safe Drinking Water Hotline at 1-800-426-4791.”
- s01-02-en-epa-boil-water · `operator_public_notice_deadline` · 锚点 1 · 第 1 页 “ATTENTION: PWS OPERATOR/RESPONSIBLE PARTY”
- s01-03-zh-holiday-2026 · `holiday_year` · 锚点 1 · 第 1 页 “现将2026年元旦、春节、清明节、劳动节、端午节、中秋节和国庆节放假调休日期的具体安排通知如下。”
- s01-03-zh-holiday-2026 · `labour_day_holiday` · 锚点 1 · 第 1 页 “四、劳动节：5月1日（周五）至5日（周二）放假调休，共5天。”
- s01-03-zh-holiday-2026 · `labour_day_makeup_workday` · 锚点 1 · 第 1 页 “共5天。5”
- s01-03-zh-holiday-2026 · `labour_day_makeup_workday` · 锚点 2 · 第 1 页 “月9日（周六）上班。”
- s01-03-zh-holiday-2026 · `mid_autumn_holiday` · 锚点 1 · 第 1 页 “六、中秋节：9月25日（周五）至27日（周日）放假，共3天。”
- s01-03-zh-holiday-2026 · `issue_date` · 锚点 1 · 第 2 页 “国务院办公厅 2025年11月4日”
- s01-04-en-govuk-sa-deadlines · `tax_year` · 锚点 1 · 第 2 页 “The last tax year started on 6 April 2025 and ended on 5 April 2026.”
- s01-04-en-govuk-sa-deadlines · `receipt_requirement` · 锚点 1 · 第 2 页 “HM Revenue and Customs (HMRC) must receive your tax return and any money you owe by the deadline.”
- s01-04-en-govuk-sa-deadlines · `profit_unknown_still_required` · 锚点 1 · 第 2 页 “If you do not know your profit for the whole tax year, you still need to send a tax return.”
- s01-04-en-govuk-sa-deadlines · `register_deadline` · 锚点 1 · 第 2 页 “You must tell HMRC by 5 October if you need to complete a tax return for the previous year and you have either:”
- s01-04-en-govuk-sa-deadlines · `register_deadline` · 锚点 2 · 第 2 页 “not sent a tax return before”
- s01-04-en-govuk-sa-deadlines · `register_deadline` · 锚点 3 · 第 2 页 “registered before but did not need to send a tax return for the tax year 2024 to 2025”
- s01-04-en-govuk-sa-deadlines · `register_deadline` · 锚点 4 · 第 2 页 “If you register after 5 October 2026”
- s01-04-en-govuk-sa-deadlines · `register_action` · 锚点 1 · 第 2 页 “You must tell HMRC by 5 October if you need to complete a tax return for the previous year and you have either:”
- s01-04-en-govuk-sa-deadlines · `register_action` · 锚点 2 · 第 2 页 “not sent a tax return before”
- s01-04-en-govuk-sa-deadlines · `register_action` · 锚点 3 · 第 2 页 “registered before but did not need to send a tax return for the tax year 2024 to 2025”
- s01-04-en-govuk-sa-deadlines · `late_registration_filing_deadline` · 锚点 1 · 第 2 页 “If you register after 5 October 2026”
- s01-04-en-govuk-sa-deadlines · `late_registration_payment_deadline` · 锚点 1 · 第 2 页 “If you register after 5 October 2026”
- s01-04-en-govuk-sa-deadlines · `late_registration_payment_deadline` · 锚点 2 · 第 2 页 “You must still pay the tax you owe by 11:59pm on 31 January 2027”
- s01-04-en-govuk-sa-deadlines · `paper_return_deadline` · 锚点 1 · 第 2 页 “Sending a paper tax return”
- s01-04-en-govuk-sa-deadlines · `paper_return_deadline` · 锚点 2 · 第 2 页 “HMRC must receive your paper tax return (/self-assessment-tax-return-forms) by 11:59pm on 31 October 2026 or you’ll get a late filing penalty”
- s01-04-en-govuk-sa-deadlines · `return_submission_open_date` · 锚点 2 · 第 3 页 “You can submit your return any time on or after 6 April 2026 up to the deadline.”
- s01-04-en-govuk-sa-deadlines · `online_return_deadline` · 锚点 1 · 第 3 页 “Submitting an online tax return”
- s01-04-en-govuk-sa-deadlines · `online_return_deadline` · 锚点 2 · 第 3 页 “You must submit your online tax return (/log-in-file-self-assessment-tax-return) by 11:59pm on 31 January 2027 or you’ll get a late filing penalty”
- s01-04-en-govuk-sa-deadlines · `tax_code_submission_deadline` · 锚点 1 · 第 3 页 “Submitting an online tax return”
- s01-04-en-govuk-sa-deadlines · `tax_code_submission_deadline` · 锚点 2 · 第 3 页 “If you want to pay your Self Assessment bill through your tax code”
- s01-04-en-govuk-sa-deadlines · `tax_code_submission_deadline` · 锚点 3 · 第 3 页 “you must submit it by 11:59pm on 30 December 2026.”
- s01-04-en-govuk-sa-deadlines · `tax_code_deadline_missed` · 锚点 1 · 第 3 页 “If you want to pay your Self Assessment bill through your tax code”
- s01-04-en-govuk-sa-deadlines · `tax_code_deadline_missed` · 锚点 2 · 第 3 页 “you must submit it by 11:59pm on 30 December 2026.”
- s01-04-en-govuk-sa-deadlines · `tax_code_deadline_missed` · 锚点 3 · 第 3 页 “If you miss this deadline, you’ll have to pay another way.”
- s01-04-en-govuk-sa-deadlines · `payment_deadline` · 锚点 1 · 第 3 页 “Paying the tax you owe”
- s01-04-en-govuk-sa-deadlines · `payment_deadline` · 锚点 2 · 第 3 页 “You need to pay your Self Assessment tax (/pay-self-assessment-tax-bill) by 11:59pm on 31 January 2027 or you’ll get a penalty”
- s01-04-en-govuk-sa-deadlines · `payments_on_account_second_deadline` · 锚点 1 · 第 3 页 “You need to pay your Self Assessment tax (/pay-self-assessment-tax-bill) by 11:59pm on 31 January 2027”
- s01-04-en-govuk-sa-deadlines · `payments_on_account_second_deadline` · 锚点 2 · 第 3 页 “There is a second payment deadline of 31 July if you make payments towards your bill - these are known as ‘payments on account”
- s01-04-en-govuk-sa-deadlines · `special_entities_paper_deadline` · 锚点 1 · 第 3 页 “Trustees of a registered pension scheme or non-resident companies”
- s01-04-en-govuk-sa-deadlines · `special_entities_cannot_file_online` · 锚点 1 · 第 3 页 “Trustees of a registered pension scheme or non-resident companies”
- s01-04-en-govuk-sa-deadlines · `special_entities_cannot_file_online` · 锚点 2 · 第 3 页 “You cannot send a return online.”
- s01-04-en-govuk-sa-deadlines · `partnership_company_partner_online_deadline` · 锚点 1 · 第 3 页 “Partnership returns if you have a company as a partner”
- s01-04-en-govuk-sa-deadlines · `partnership_company_partner_online_deadline` · 锚点 2 · 第 4 页 “If your partnership’s accounting date is between 1 February and 5 April and one of your partners is a limited company, the deadline for:”
- s01-04-en-govuk-sa-deadlines · `partnership_company_partner_paper_deadline` · 锚点 1 · 第 3 页 “Partnership returns if you have a company as a partner”
- s01-04-en-govuk-sa-deadlines · `partnership_company_partner_paper_deadline` · 锚点 2 · 第 4 页 “If your partnership’s accounting date is between 1 February and 5 April and one of your partners is a limited company, the deadline for:”
- s01-05-zh-putuo-fee-2024 · `dog_fee_inner_ring` · 锚点 1 · 第 9 页 “19 犬类管理费”
- s01-05-zh-putuo-fee-2024 · `dog_fee_inner_ring` · 锚点 2 · 第 9 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `dog_fee_inner_ring` · 锚点 3 · 第 9 页 “内环以内区域500元/犬.年”
- s01-05-zh-putuo-fee-2024 · `dog_fee_outer_ring` · 锚点 1 · 第 9 页 “19 犬类管理费”
- s01-05-zh-putuo-fee-2024 · `dog_fee_outer_ring` · 锚点 2 · 第 9 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `dog_fee_outer_ring` · 锚点 3 · 第 9 页 “内环以外区域300元/犬.年(农村地区停征)”
- s01-05-zh-putuo-fee-2024 · `rural_dog_fee_suspension` · 锚点 1 · 第 9 页 “19 犬类管理费”
- s01-05-zh-putuo-fee-2024 · `rural_dog_fee_suspension` · 锚点 2 · 第 9 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `rural_dog_fee_suspension` · 锚点 3 · 第 9 页 “内环以外区域300元/犬.年(农村地区停征)”
- s01-05-zh-putuo-fee-2024 · `residence_card_lost_or_damaged_replacement_fee` · 锚点 1 · 第 9 页 “20 上海市居住证工本费(限丢失、损坏补办)”（`find` 定位不到）
- s01-05-zh-putuo-fee-2024 · `residence_card_lost_or_damaged_replacement_fee` · 锚点 2 · 第 9 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `residence_card_lost_or_damaged_replacement_fee` · 锚点 3 · 第 9 页 “上海市居住证工本费(限丢失、损坏补办) 20元/证”（`find` 定位不到）
- s01-05-zh-putuo-fee-2024 · `id_card_loss_replacement_fee` · 锚点 1 · 第 7 页 “①居民身份证”
- s01-05-zh-putuo-fee-2024 · `id_card_loss_replacement_fee` · 锚点 2 · 第 7 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `id_card_loss_replacement_fee` · 锚点 3 · 第 7 页 “因丢失损换等换、补领40元/证”
- s01-05-zh-putuo-fee-2024 · `id_card_first_issue_exemption` · 锚点 1 · 第 7 页 “(4)居民身份证工本费”
- s01-05-zh-putuo-fee-2024 · `id_card_first_issue_exemption` · 锚点 2 · 第 7 页 “备注”
- s01-05-zh-putuo-fee-2024 · `id_card_first_issue_exemption` · 锚点 3 · 第 7 页 “停征首次申领居民身份证工本费”
- s01-05-zh-putuo-fee-2024 · `passport_fee` · 锚点 1 · 第 8 页 “(5)公民出入境证件费”
- s01-05-zh-putuo-fee-2024 · `passport_fee` · 锚点 2 · 第 8 页 “①护照 120元/本”
- s01-05-zh-putuo-fee-2024 · `passport_fee` · 锚点 3 · 第 8 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_level1_fee` · 锚点 1 · 第 3 页 “(1)公办幼儿园保育教育费”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_level1_fee` · 锚点 2 · 第 3 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_level1_fee` · 锚点 3 · 第 3 页 “全日制一级园225元/月、二级园175元/月、三级园125元/月、市级示范园700元/月；”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_level2_fee` · 锚点 1 · 第 3 页 “(1)公办幼儿园保育教育费”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_level2_fee` · 锚点 2 · 第 3 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_level2_fee` · 锚点 3 · 第 3 页 “全日制一级园225元/月、二级园175元/月、三级园125元/月、市级示范园700元/月；”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_level3_fee` · 锚点 1 · 第 3 页 “(1)公办幼儿园保育教育费”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_level3_fee` · 锚点 2 · 第 3 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_level3_fee` · 锚点 3 · 第 3 页 “全日制一级园225元/月、二级园175元/月、三级园125元/月、市级示范园700元/月；”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_model_fee` · 锚点 1 · 第 3 页 “(1)公办幼儿园保育教育费”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_model_fee` · 锚点 2 · 第 3 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_full_day_model_fee` · 锚点 3 · 第 3 页 “全日制一级园225元/月、二级园175元/月、三级园125元/月、市级示范园700元/月；”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_boarding_level1_fee` · 锚点 1 · 第 3 页 “(1)公办幼儿园保育教育费”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_boarding_level1_fee` · 锚点 2 · 第 3 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_boarding_level1_fee` · 锚点 3 · 第 3 页 “寄宿制一级园390元/月、二级园340元/月、三级园290元/月、市级示范园800元/月”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_boarding_level2_fee` · 锚点 1 · 第 3 页 “(1)公办幼儿园保育教育费”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_boarding_level2_fee` · 锚点 2 · 第 3 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_boarding_level2_fee` · 锚点 3 · 第 3 页 “寄宿制一级园390元/月、二级园340元/月、三级园290元/月、市级示范园800元/月”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_boarding_level3_fee` · 锚点 1 · 第 3 页 “(1)公办幼儿园保育教育费”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_boarding_level3_fee` · 锚点 2 · 第 3 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_boarding_level3_fee` · 锚点 3 · 第 3 页 “寄宿制一级园390元/月、二级园340元/月、三级园290元/月、市级示范园800元/月”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_fee_approval_number` · 锚点 1 · 第 3 页 “(1)公办幼儿园保育教育费”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_fee_approval_number` · 锚点 2 · 第 3 页 “批准文号”
- s01-05-zh-putuo-fee-2024 · `public_kindergarten_fee_approval_number` · 锚点 3 · 第 3 页 “沪价费[2012]009号”
- s01-05-zh-putuo-fee-2024 · `foreign_invested_enterprise_site_use_fee` · 锚点 1 · 第 2 页 “6 外商投资企业场地使用费”
- s01-05-zh-putuo-fee-2024 · `foreign_invested_enterprise_site_use_fee` · 锚点 2 · 第 2 页 “收费标准”
- s01-05-zh-putuo-fee-2024 · `foreign_invested_enterprise_site_use_fee` · 锚点 3 · 第 2 页 “0.50-170元/平方米.年，详见文件”
- s01-05-zh-putuo-fee-2024 · `exam_fee_table_reference` · 锚点 1 · 第 10 页 “3.后附表《上海市考试考务费目录清单》。”
- s01-06-zh-holiday-2026-scan · `holiday_year` · 锚点 1 · 第 1 页 “现将2026年元旦、春节、清明节、劳动节、端午节、中秋节和国庆节放假调休日期的具体安排通知如下。”
- s01-06-zh-holiday-2026-scan · `new_year_holiday` · 锚点 1 · 第 1 页 “一、元旦：1月1日（周四）至3日（周六）放假调休，共3天。”（`find` 定位不到）
- s01-06-zh-holiday-2026-scan · `new_year_makeup_workday` · 锚点 1 · 第 1 页 “共3天。1月4日（周日）上班。”
- s01-06-zh-holiday-2026-scan · `spring_festival_holiday` · 锚点 1 · 第 1 页 “二、春节：2月15日（农历腊月二十八、周日）至23日（农历正月初七、周一）放假调休，共9天。”（`find` 定位不到）
- s01-06-zh-holiday-2026-scan · `spring_festival_makeup_workdays` · 锚点 1 · 第 1 页 “共9天。2月14日（周六）、2月28日（周六）上班。”
- s01-06-zh-holiday-2026-scan · `qingming_holiday` · 锚点 1 · 第 1 页 “三、清明节：4月4日（周六）至6日（周一）放假，共3天。”
- s01-06-zh-holiday-2026-scan · `labour_day_holiday` · 锚点 1 · 第 1 页 “四、劳动节：5月1日（周五）至5日（周二）放假调休，共5天。”
- s01-06-zh-holiday-2026-scan · `dragon_boat_holiday` · 锚点 1 · 第 1 页 “五、端午节：6月19日（周五）至21日（周日）放假，共3天。”
- s01-06-zh-holiday-2026-scan · `mid_autumn_holiday` · 锚点 1 · 第 1 页 “六、中秋节：9月25日（周五）至27日（周日）放假，共3天。”
- s01-06-zh-holiday-2026-scan · `national_day_holiday` · 锚点 1 · 第 1 页 “七、国庆节：10月1日（周四）至7日（周三）放假调休，共7天。”（`find` 定位不到）
- s01-06-zh-holiday-2026-scan · `national_day_makeup_workdays` · 锚点 1 · 第 1 页 “七、国庆节：10月1日（周四）至7日（周三）放假调休，共7天。9月20日（周日）、10月10日（周六）上班。”（`find` 定位不到）
- s01-07-mixed-building-notice · `notice_reference` · 锚点 1 · 第 1 页 “通知编号 Ref. No.: FM-2026-0042”
- s01-07-mixed-building-notice · `shutoff_start_time` · 锚点 1 · 第 1 页 “将于2026年10月12日（周一）9:00至18:00暂停供水。”
- s01-07-mixed-building-notice · `shutoff_start_time` · 锚点 3 · 第 1 页 “时间 Time”
- s01-07-mixed-building-notice · `shutoff_start_time` · 锚点 5 · 第 1 页 “from 9:00 to 17:00.”
- s01-07-mixed-building-notice · `affected_units` · 锚点 1 · 第 1 页 “停水 Water shut-off”
- s01-07-mixed-building-notice · `affected_units` · 锚点 2 · 第 1 页 “范围 Scope”
- s01-07-mixed-building-notice · `affected_units` · 锚点 3 · 第 1 页 “9:00–18:00 B座 0302–2208 单元”
- s01-07-mixed-building-notice · `shutoff_registration_not_needed` · 锚点 1 · 第 1 页 “本次停水无需”
- s01-07-mixed-building-notice · `shutoff_registration_not_needed` · 锚点 2 · 第 1 页 “提前登记；”
- s01-07-mixed-building-notice · `shutoff_registration_not_needed` · 锚点 3 · 第 1 页 “You do not need to register for the shut-off itself.”
- s01-07-mixed-building-notice · `drinking_water_registration_deadline` · 锚点 1 · 第 1 页 “如需在停水期间使用 Facility Management（FM）提供的临时饮用水，请于”
- s01-07-mixed-building-notice · `drinking_water_registration_deadline` · 锚点 2 · 第 1 页 “10月10日17:00前在物业前台登记”
- s01-07-mixed-building-notice · `drinking_water_registration_deadline` · 锚点 3 · 第 1 页 “临时饮用水登记 Water registration”
- s01-07-mixed-building-notice · `drinking_water_registration_deadline` · 锚点 4 · 第 1 页 “日期 Date”
- s01-07-mixed-building-notice · `drinking_water_registration_deadline` · 锚点 5 · 第 1 页 “时间 Time”
- s01-07-mixed-building-notice · `drinking_water_registration_deadline` · 锚点 6 · 第 1 页 “截止 10月10日 17:00 物业前台 FM front desk”
- s01-07-mixed-building-notice · `drinking_water_registration_deadline` · 锚点 7 · 第 1 页 “To receive temporary drinking water from Facility Management, register at the FM front desk by 17:00 on 10 October”
- s01-07-mixed-building-notice · `drinking_water_registration_location` · 锚点 1 · 第 1 页 “如需在停水期间使用 Facility Management（FM）提供的临时饮用水，请于”
- s01-07-mixed-building-notice · `drinking_water_registration_location` · 锚点 2 · 第 1 页 “10月10日17:00前在物业前台登记”
- s01-07-mixed-building-notice · `drinking_water_registration_location` · 锚点 3 · 第 1 页 “To receive temporary drinking water from Facility Management, register at the FM front desk”
- s01-07-mixed-building-notice · `drinking_water_limit` · 锚点 1 · 第 1 页 “如需在停水期间使用 Facility Management（FM）提供的临时饮用水，请于”
- s01-07-mixed-building-notice · `drinking_water_limit` · 锚点 2 · 第 1 页 “户限领 2 桶（18.9L/桶）”
- s01-07-mixed-building-notice · `drinking_water_limit` · 锚点 3 · 第 1 页 “To receive temporary drinking water from Facility Management”
- s01-07-mixed-building-notice · `drinking_water_limit` · 锚点 4 · 第 1 页 “(limit: 2 bottles of 18.9 L per unit)”
- s01-07-mixed-building-notice · `store_water_in_advance` · 锚点 2 · 第 1 页 “Please store water in advance.”
- s01-07-mixed-building-notice · `lobby_restroom_closure` · 锚点 1 · 第 1 页 “停水期间，Lobby 一”
- s01-07-mixed-building-notice · `lobby_restroom_closure` · 锚点 3 · 第 1 页 “The ground-floor Lobby restrooms will also be closed.”
- s01-07-mixed-building-notice · `lifts_under_maintenance` · 锚点 1 · 第 1 页 “2号、3号电梯进行年度维保（annual maintenance）”
- s01-07-mixed-building-notice · `lifts_under_maintenance` · 锚点 2 · 第 1 页 “Lifts 2 and 3 will undergo annual maintenance on the same day;”
- s01-07-mixed-building-notice · `lift_in_service` · 锚点 1 · 第 1 页 “1号电梯照常运行。”
- s01-07-mixed-building-notice · `lift_in_service` · 锚点 2 · 第 1 页 “Lift 1 remains in service.”
- s01-07-mixed-building-notice · `lift_maintenance_date` · 锚点 2 · 第 1 页 “日期 Date”
- s01-07-mixed-building-notice · `lift_maintenance_duration` · 锚点 2 · 第 1 页 “时间 Time”
- s01-07-mixed-building-notice · `flush_after_restoration` · 锚点 1 · 第 1 页 “停水恢复后可能出现短时水浑，属正常现象，请放水 3–5 分钟”
- s01-07-mixed-building-notice · `flush_after_restoration` · 锚点 2 · 第 1 页 “后再使用。”
- s01-07-mixed-building-notice · `contact_party` · 锚点 1 · 第 1 页 “如有疑问，请联系物业服务”

`must_not_assert`（未评审的）：

- s01-01-zh-sh-fee-2025 · 第 1 条：个人缴费有某个具体截止日期
- s01-01-zh-sh-fee-2025 · 第 2 条：文件给出了办理地址
- s01-01-zh-sh-fee-2025 · 第 3 条：文件给出了咨询电话
- s01-01-zh-sh-fee-2025 · 第 4 条：所有居民身份证的申领、换领、补领都免费
- s01-01-zh-sh-fee-2025 · 第 5 条：台湾居民来往大陆通行证的换发也免收证件费
- s01-01-zh-sh-fee-2025 · 第 6 条：台湾居民来往大陆通行证的签注也免收费用
- s01-01-zh-sh-fee-2025 · 第 7 条：其他涉台证件也免收证件费
- s01-01-zh-sh-fee-2025 · 第 8 条：《收费目录》包括中央在沪单位执收的行政事业性收费
- s01-01-zh-sh-fee-2025 · 第 9 条：《收费目录》公布后不会再新增、取消或调整收费事项
- s01-01-zh-sh-fee-2025 · 第 10 条：文件的成文日期是2025年12月24日
- s01-01-zh-sh-fee-2025 · 第 11 条：身份证收费一栏的原文写的是“损坏”
- s01-02-en-epa-boil-water · 第 1 条：the notice names the affected water system
- s01-02-en-epa-boil-water · 第 2 条：the notice names the affected locality
- s01-02-en-epa-boil-water · 第 3 条：the contamination was learned of on a specific date
- s01-02-en-epa-boil-water · 第 4 条：the samples were collected on a specific date
- s01-02-en-epa-boil-water · 第 5 条：the problem is expected to be resolved within a specific time frame
- s01-02-en-epa-boil-water · 第 6 条：the notice gives a contact name for the local water system
- s01-02-en-epa-boil-water · 第 7 条：the notice gives a phone number for the local water system
- s01-02-en-epa-boil-water · 第 8 条：the notice gives a mailing address for the local water system
- s01-02-en-epa-boil-water · 第 9 条：specific corrective measures (such as chlorination or flushing) are under way（除非：presents them as example actions listed for operators）
- s01-02-en-epa-boil-water · 第 10 条：an alternative-water location has been arranged
- s01-02-en-epa-boil-water · 第 11 条：the water is already safe to drink without boiling
- s01-02-en-epa-boil-water · 第 12 条：bottled water must also be boiled
- s01-02-en-epa-boil-water · 第 13 条：residents must notify others within 24 hours
- s01-02-en-epa-boil-water · 第 14 条：residents must contact EPA
- s01-02-en-epa-boil-water · 第 15 条：residents must sign the certification of public notification
- s01-02-en-epa-boil-water · 第 16 条：residents must see a doctor
- s01-03-zh-holiday-2026 · 第 1 条：本通知为2026年清明节统一安排了调休上班日
- s01-03-zh-holiday-2026 · 第 2 条：本通知为2026年端午节统一安排了调休上班日
- s01-03-zh-holiday-2026 · 第 3 条：本通知为2026年中秋节统一安排了调休上班日
- s01-03-zh-holiday-2026 · 第 4 条：本通知统一安排的调休上班日不止 1月4日、2月14日、2月28日、5月9日、9月20日、10月10日 这六天
- s01-03-zh-holiday-2026 · 第 5 条：这份通知安排的是2025年的节假日
- s01-03-zh-holiday-2026 · 第 6 条：本通知要求单位或个人必须使用带薪年休假
- s01-03-zh-holiday-2026 · 第 7 条：本通知保证每个人在放假期间都不用上班
- s01-04-en-govuk-sa-deadlines · 第 1 条：a specific penalty amount
- s01-04-en-govuk-sa-deadlines · 第 2 条：every paper return is due by 31 October 2026
- s01-04-en-govuk-sa-deadlines · 第 3 条：trustees of a registered pension scheme can file online
- s01-04-en-govuk-sa-deadlines · 第 4 条：non-resident companies can file online
- s01-04-en-govuk-sa-deadlines · 第 5 条：registering after 5 October 2026 also extends the deadline for paying the tax
- s01-04-en-govuk-sa-deadlines · 第 6 条：a specific calendar date for the late-registration filing deadline（除非：computed from a letter or email date the user supplied）
- s01-04-en-govuk-sa-deadlines · 第 7 条：a specific calendar date for the partnership online-return deadline（除非：computed from an accounting date the user supplied）
- s01-04-en-govuk-sa-deadlines · 第 8 条：a specific calendar date for the partnership paper-return deadline（除非：computed from an accounting date the user supplied）
- s01-04-en-govuk-sa-deadlines · 第 9 条：the 31 July payment deadline falls in a year other than 2027
- s01-04-en-govuk-sa-deadlines · 第 10 条：people who do not know their profit for the whole year do not need to send a return
- s01-04-en-govuk-sa-deadlines · 第 11 条：posting the paper return by the deadline is enough
- s01-05-zh-putuo-fee-2024 · 第 1 条：清单写明了考试考务费的具体金额
- s01-05-zh-putuo-fee-2024 · 第 2 条：清单写明了成文日期
- s01-05-zh-putuo-fee-2024 · 第 3 条：清单写明了发布日期
- s01-05-zh-putuo-fee-2024 · 第 4 条：这是 2025 年度的收费目录
- s01-05-zh-putuo-fee-2024 · 第 5 条：所有上海市居住证的办理（含首次办理）都收 20 元/证
- s01-05-zh-putuo-fee-2024 · 第 6 条：农村地区也要缴纳犬类管理费
- s01-05-zh-putuo-fee-2024 · 第 7 条：所有居民身份证的申领、换领、补领都免费
- s01-05-zh-putuo-fee-2024 · 第 8 条：本清单规定了 2025–2027 年首次申领台湾居民来往大陆通行证免收证件费
- s01-05-zh-putuo-fee-2024 · 第 9 条：文件给出了办理地址
- s01-05-zh-putuo-fee-2024 · 第 10 条：文件给出了咨询电话
- s01-06-zh-holiday-2026-scan · 第 1 条：本通知为2026年清明节统一安排了调休上班日
- s01-06-zh-holiday-2026-scan · 第 2 条：本通知为2026年端午节统一安排了调休上班日
- s01-06-zh-holiday-2026-scan · 第 3 条：本通知为2026年中秋节统一安排了调休上班日
- s01-06-zh-holiday-2026-scan · 第 4 条：本通知统一安排的调休上班日不止 1月4日、2月14日、2月28日、5月9日、9月20日、10月10日 这六天
- s01-06-zh-holiday-2026-scan · 第 5 条：这份通知安排的是2025年的节假日
- s01-06-zh-holiday-2026-scan · 第 6 条：这张图上写有成文日期
- s01-06-zh-holiday-2026-scan · 第 7 条：这张图上写有发布日期
- s01-06-zh-holiday-2026-scan · 第 8 条：这张图上写有带薪年休假的安排
- s01-06-zh-holiday-2026-scan · 第 9 条：这张图上写有值班安排
- s01-07-mixed-building-notice · 第 1 条：通知给出了物业服务中心的联系电话
- s01-07-mixed-building-notice · 第 2 条：停水本身需要提前登记
- s01-07-mixed-building-notice · 第 3 条：领取临时饮用水不需要登记
- s01-07-mixed-building-notice · 第 4 条：停水日期是确定的某一天
- s01-07-mixed-building-notice · 第 5 条：停水结束时间是确定的某个时刻
- s01-07-mixed-building-notice · 第 6 条：停水持续的具体时长（除非：在同一处说法内分别计算（中文正文 12 日 9:00–18:00、英文 12 日 9:00–17:00、表格 13 日 9:00–18:00），并指出它们互相冲突；不能把不同说法的日期和时刻拼在一起）
- s01-07-mixed-building-notice · 第 7 条：所有电梯（包括 1 号电梯）都停用
- s01-07-mixed-building-notice · 第 8 条：通知写明了临时饮用水的领取时间
- s01-07-mixed-building-notice · 第 9 条：通知写明了临时饮用水的领取地点
- s01-07-mixed-building-notice · 第 10 条：恢复供水后可以不放水直接使用
- s01-07-mixed-building-notice · 第 11 条：B座 0302–2208 以外的单元不会停水
- s01-08-zh-shtax-iit-2025 · 第 1 条：3月21日至6月30日期间办理也需要预约
- s01-08-zh-shtax-iit-2025 · 第 2 条：3月1日至3月20日期间不预约就不能办理
- s01-08-zh-shtax-iit-2025 · 第 3 条：本通告规定的办理期间延续到2026年6月30日之后
- s01-08-zh-shtax-iit-2025 · 第 4 条：通告说明了哪些纳税人需要办理汇算
- s01-08-zh-shtax-iit-2025 · 第 5 条：通告写明了逾期未办理的后果
- s01-08-zh-shtax-iit-2025 · 第 6 条：通告写明了逾期未办理的罚款金额
- s01-08-zh-shtax-iit-2025 · 第 7 条：这份通告的发文单位是国家税务总局上海市税务局
- s01-08-zh-shtax-iit-2025 · 第 8 条：通告的成文日期是2026年2月10日
- s01-08-zh-shtax-iit-2025 · 第 9 条：通告正文给出了咨询电话

