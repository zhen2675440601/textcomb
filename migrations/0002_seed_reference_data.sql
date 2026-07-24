INSERT INTO evidence_entries(id, title, revision, source_url, redistributable)
VALUES
    (
        'gb-t-15834-2011',
        '标点符号用法',
        'GB/T 15834-2011',
        'https://openstd.samr.gov.cn/bzgk/std/newGbInfo?hcno=22EA6D162E4110E752259661E1A0D0A8',
        FALSE
    ),
    (
        'gb-t-15835-2011',
        '出版物上数字用法',
        'GB/T 15835-2011',
        'https://std.samr.gov.cn/gb/search/gbDetailed?id=71F772D7DA87D3A7E05397BE0A0AB82A',
        FALSE
    ),
    (
        'standard-chinese-characters-2013',
        '通用规范汉字表',
        '2013',
        'https://www.moe.gov.cn/jyb_sjzl/ziliao/A19/201306/t20130601_186002.html',
        FALSE
    )
ON CONFLICT (id) DO NOTHING;
