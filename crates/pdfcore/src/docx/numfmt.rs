//! 编号的数字格式（`w:numFmt`）：把第几项写成「一」「①」「IV」「壹拾」。
//!
//! 取值对照 LibreOffice 24.2 实测（每种格式 1..120）。有三种格式 LibreOffice 画成了
//! 普通数字 —— 全角数字、⑴、⒈ —— 这三种按规范实现，与 Word 一致。

/// `n` 按 `fmt` 写成文字。不认识的格式返回 None，由调用方按阿拉伯数字输出并提示。
/// `bullet` 的文字就是 `w:lvlText` 本身，这里给空串。
pub fn format(n: i32, fmt: &str) -> Option<String> {
    let decimal = || n.to_string();
    let u = n.max(0) as u32;
    Some(match fmt {
        "decimal" | "decimalHalfWidth" => decimal(),
        "decimalZero" if (0..10).contains(&n) => format!("0{n}"),
        "decimalZero" => decimal(),
        "decimalFullWidth" => decimal()
            .chars()
            .map(|c| match c.to_digit(10) {
                Some(d) => char::from_u32(0xFF10 + d).unwrap_or(c),
                None => c,
            })
            .collect(),
        "upperRoman" if n > 0 => roman(u),
        "lowerRoman" if n > 0 => roman(u).to_lowercase(),
        "upperLetter" if n > 0 => letters(u, b'A'),
        "lowerLetter" if n > 0 => letters(u, b'a'),
        "upperRoman" | "lowerRoman" | "upperLetter" | "lowerLetter" => decimal(),
        "ordinal" => format!("{n}{}", ordinal_suffix(u)),
        "cardinalText" => capitalize(&words(u)),
        "ordinalText" => capitalize(&ordinal_words(&words(u))),
        // LibreOffice 在 ⑳ 之后接着用 ㉑…㊿，再往后才是普通数字。
        "decimalEnclosedCircle" | "decimalEnclosedCircleChinese" | "ideographEnclosedCircle" => {
            match u {
                1..=20 => enclosed(0x2460, u - 1),
                21..=35 => enclosed(0x3251, u - 21),
                36..=50 => enclosed(0x32B1, u - 36),
                _ => decimal(),
            }
        }
        "decimalEnclosedParen" if (1..=20).contains(&u) => enclosed(0x2474, u - 1),
        "decimalEnclosedFullstop" if (1..=20).contains(&u) => enclosed(0x2488, u - 1),
        "decimalEnclosedParen" | "decimalEnclosedFullstop" => decimal(),
        "chineseCounting"
        | "chineseCountingThousand"
        | "ideographDigital"
        | "japaneseCounting"
        | "taiwaneseCounting"
        | "taiwaneseCountingThousand" => chinese(u, &COUNTING),
        "chineseLegalSimplified" => chinese(u, &LEGAL_SIMPLIFIED),
        "ideographLegalTraditional" => chinese(u, &LEGAL_TRADITIONAL),
        "ideographTraditional" => cycle(u, "甲乙丙丁戊己庚辛壬癸").unwrap_or_else(decimal),
        "ideographZodiac" => cycle(u, "子丑寅卯辰巳午未申酉戌亥").unwrap_or_else(decimal),
        "none" | "bullet" => String::new(),
        _ => return None,
    })
}

fn enclosed(base: u32, k: u32) -> String {
    char::from_u32(base + k)
        .map(String::from)
        .unwrap_or_default()
}

/// 第 `n` 个字（从 1 数）；超出这一轮就没有。
fn cycle(n: u32, chars: &str) -> Option<String> {
    let i = n.checked_sub(1)? as usize;
    chars.chars().nth(i).map(String::from)
}

fn roman(mut n: u32) -> String {
    const TABLE: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut s = String::new();
    for (v, r) in TABLE {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

/// A…Z 之后是 AA、BB……ZZ，再是 AAA：同一个字母重复，不是进位。
fn letters(n: u32, base: u8) -> String {
    let c = (base + ((n - 1) % 26) as u8) as char;
    std::iter::repeat_n(c, ((n - 1) / 26 + 1) as usize).collect()
}

fn ordinal_suffix(n: u32) -> &'static str {
    match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    }
}

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// 英文数词，小写：「twenty-one」「one hundred one」（LibreOffice 的写法，不加 and）。
fn words(n: u32) -> String {
    fn below_1000(n: u32) -> String {
        let tens = |n: u32| match n {
            0..=19 => ONES[n as usize].to_string(),
            _ if n.is_multiple_of(10) => TENS[(n / 10) as usize].to_string(),
            _ => format!("{}-{}", TENS[(n / 10) as usize], ONES[(n % 10) as usize]),
        };
        match (n / 100, n % 100) {
            (0, r) => tens(r),
            (h, 0) => format!("{} hundred", ONES[h as usize]),
            (h, r) => format!("{} hundred {}", ONES[h as usize], tens(r)),
        }
    }
    let mut parts = Vec::new();
    for (scale, name) in [
        (1_000_000_000, "billion"),
        (1_000_000, "million"),
        (1_000, "thousand"),
    ] {
        if !(n / scale).is_multiple_of(1000) {
            parts.push(format!("{} {name}", below_1000(n / scale % 1000)));
        }
    }
    if !n.is_multiple_of(1000) || parts.is_empty() {
        parts.push(below_1000(n % 1000));
    }
    parts.join(" ")
}

/// 把数词的最后一个词换成序数词：「twenty-one」→「twenty-first」。
fn ordinal_words(cardinal: &str) -> String {
    let cut = cardinal.rfind([' ', '-']).map_or(0, |i| i + 1);
    let (head, last) = cardinal.split_at(cut);
    let last = match last {
        "one" => "first".to_string(),
        "two" => "second".to_string(),
        "three" => "third".to_string(),
        "five" => "fifth".to_string(),
        "eight" => "eighth".to_string(),
        "nine" => "ninth".to_string(),
        "twelve" => "twelfth".to_string(),
        w if w.ends_with('y') => format!("{}ieth", &w[..w.len() - 1]),
        w => format!("{w}th"),
    };
    format!("{head}{last}")
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// 中文数字的一套写法。
struct Chinese {
    /// 0–9。单独一个 0 写 `digits[0]`。
    digits: [char; 10],
    /// 数中间的零：计数写法单独的 0 是「〇」，中间的零却读「零」。
    zero: char,
    /// 十、百、千。
    units: [char; 3],
    wan: char,
    /// 十一到十九省掉开头的「一」（「十一」而不是「一十一」）。大写数字不省（「壹拾壹」）。
    bare_ten: bool,
}

const COUNTING: Chinese = Chinese {
    digits: ['〇', '一', '二', '三', '四', '五', '六', '七', '八', '九'],
    zero: '零',
    units: ['十', '百', '千'],
    wan: '万',
    bare_ten: true,
};
const LEGAL_SIMPLIFIED: Chinese = Chinese {
    digits: ['零', '壹', '贰', '叁', '肆', '伍', '陆', '柒', '捌', '玖'],
    zero: '零',
    units: ['拾', '佰', '仟'],
    wan: '万',
    bare_ten: false,
};
const LEGAL_TRADITIONAL: Chinese = Chinese {
    digits: ['零', '壹', '貳', '參', '肆', '伍', '陸', '柒', '捌', '玖'],
    zero: '零',
    units: ['拾', '佰', '仟'],
    wan: '萬',
    bare_ten: false,
};

/// 中文读法：一百零一、一千零一十、一万零一。中间连续的零只读一个「零」，末尾的零不读。
fn chinese(n: u32, c: &Chinese) -> String {
    if n == 0 {
        return c.digits[0].to_string();
    }
    // 0..9999 的一节。`leading` 是整个数的开头（只有开头的「一十」才省成「十」）。
    let section = |v: u32, leading: bool, out: &mut String| {
        let ds = [v / 1000, v / 100 % 10, v / 10 % 10, v % 10];
        let (mut started, mut zero) = (false, false);
        for (i, &d) in ds.iter().enumerate() {
            if d == 0 {
                zero |= started;
                continue;
            }
            if zero {
                out.push(c.zero);
                zero = false;
            }
            if !(c.bare_ten && leading && !started && i == 2 && d == 1) {
                out.push(c.digits[d as usize]);
            }
            if i < 3 {
                out.push(c.units[2 - i]);
            }
            started = true;
        }
    };
    let (high, low) = (n / 10_000, n % 10_000);
    let mut s = String::new();
    if high > 0 {
        section(high, true, &mut s);
        s.push(c.wan);
        if (1..1000).contains(&low) {
            s.push(c.zero);
        }
    }
    if low > 0 {
        section(low, high == 0, &mut s);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LibreOffice 24.2 排出来的 1..120，逐项对照。
    const GOLDEN: &[(&str, &str)] = &[
        (
            "decimal",
            "1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95,96,97,98,99,100,101,102,103,104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120",
        ),
        (
            "upperRoman",
            "I,II,III,IV,V,VI,VII,VIII,IX,X,XI,XII,XIII,XIV,XV,XVI,XVII,XVIII,XIX,XX,XXI,XXII,XXIII,XXIV,XXV,XXVI,XXVII,XXVIII,XXIX,XXX,XXXI,XXXII,XXXIII,XXXIV,XXXV,XXXVI,XXXVII,XXXVIII,XXXIX,XL,XLI,XLII,XLIII,XLIV,XLV,XLVI,XLVII,XLVIII,XLIX,L,LI,LII,LIII,LIV,LV,LVI,LVII,LVIII,LIX,LX,LXI,LXII,LXIII,LXIV,LXV,LXVI,LXVII,LXVIII,LXIX,LXX,LXXI,LXXII,LXXIII,LXXIV,LXXV,LXXVI,LXXVII,LXXVIII,LXXIX,LXXX,LXXXI,LXXXII,LXXXIII,LXXXIV,LXXXV,LXXXVI,LXXXVII,LXXXVIII,LXXXIX,XC,XCI,XCII,XCIII,XCIV,XCV,XCVI,XCVII,XCVIII,XCIX,C,CI,CII,CIII,CIV,CV,CVI,CVII,CVIII,CIX,CX,CXI,CXII,CXIII,CXIV,CXV,CXVI,CXVII,CXVIII,CXIX,CXX",
        ),
        (
            "lowerRoman",
            "i,ii,iii,iv,v,vi,vii,viii,ix,x,xi,xii,xiii,xiv,xv,xvi,xvii,xviii,xix,xx,xxi,xxii,xxiii,xxiv,xxv,xxvi,xxvii,xxviii,xxix,xxx,xxxi,xxxii,xxxiii,xxxiv,xxxv,xxxvi,xxxvii,xxxviii,xxxix,xl,xli,xlii,xliii,xliv,xlv,xlvi,xlvii,xlviii,xlix,l,li,lii,liii,liv,lv,lvi,lvii,lviii,lix,lx,lxi,lxii,lxiii,lxiv,lxv,lxvi,lxvii,lxviii,lxix,lxx,lxxi,lxxii,lxxiii,lxxiv,lxxv,lxxvi,lxxvii,lxxviii,lxxix,lxxx,lxxxi,lxxxii,lxxxiii,lxxxiv,lxxxv,lxxxvi,lxxxvii,lxxxviii,lxxxix,xc,xci,xcii,xciii,xciv,xcv,xcvi,xcvii,xcviii,xcix,c,ci,cii,ciii,civ,cv,cvi,cvii,cviii,cix,cx,cxi,cxii,cxiii,cxiv,cxv,cxvi,cxvii,cxviii,cxix,cxx",
        ),
        (
            "upperLetter",
            "A,B,C,D,E,F,G,H,I,J,K,L,M,N,O,P,Q,R,S,T,U,V,W,X,Y,Z,AA,BB,CC,DD,EE,FF,GG,HH,II,JJ,KK,LL,MM,NN,OO,PP,QQ,RR,SS,TT,UU,VV,WW,XX,YY,ZZ,AAA,BBB,CCC,DDD,EEE,FFF,GGG,HHH,III,JJJ,KKK,LLL,MMM,NNN,OOO,PPP,QQQ,RRR,SSS,TTT,UUU,VVV,WWW,XXX,YYY,ZZZ,AAAA,BBBB,CCCC,DDDD,EEEE,FFFF,GGGG,HHHH,IIII,JJJJ,KKKK,LLLL,MMMM,NNNN,OOOO,PPPP,QQQQ,RRRR,SSSS,TTTT,UUUU,VVVV,WWWW,XXXX,YYYY,ZZZZ,AAAAA,BBBBB,CCCCC,DDDDD,EEEEE,FFFFF,GGGGG,HHHHH,IIIII,JJJJJ,KKKKK,LLLLL,MMMMM,NNNNN,OOOOO,PPPPP",
        ),
        (
            "lowerLetter",
            "a,b,c,d,e,f,g,h,i,j,k,l,m,n,o,p,q,r,s,t,u,v,w,x,y,z,aa,bb,cc,dd,ee,ff,gg,hh,ii,jj,kk,ll,mm,nn,oo,pp,qq,rr,ss,tt,uu,vv,ww,xx,yy,zz,aaa,bbb,ccc,ddd,eee,fff,ggg,hhh,iii,jjj,kkk,lll,mmm,nnn,ooo,ppp,qqq,rrr,sss,ttt,uuu,vvv,www,xxx,yyy,zzz,aaaa,bbbb,cccc,dddd,eeee,ffff,gggg,hhhh,iiii,jjjj,kkkk,llll,mmmm,nnnn,oooo,pppp,qqqq,rrrr,ssss,tttt,uuuu,vvvv,wwww,xxxx,yyyy,zzzz,aaaaa,bbbbb,ccccc,ddddd,eeeee,fffff,ggggg,hhhhh,iiiii,jjjjj,kkkkk,lllll,mmmmm,nnnnn,ooooo,ppppp",
        ),
        (
            "ordinal",
            "1st,2nd,3rd,4th,5th,6th,7th,8th,9th,10th,11th,12th,13th,14th,15th,16th,17th,18th,19th,20th,21st,22nd,23rd,24th,25th,26th,27th,28th,29th,30th,31st,32nd,33rd,34th,35th,36th,37th,38th,39th,40th,41st,42nd,43rd,44th,45th,46th,47th,48th,49th,50th,51st,52nd,53rd,54th,55th,56th,57th,58th,59th,60th,61st,62nd,63rd,64th,65th,66th,67th,68th,69th,70th,71st,72nd,73rd,74th,75th,76th,77th,78th,79th,80th,81st,82nd,83rd,84th,85th,86th,87th,88th,89th,90th,91st,92nd,93rd,94th,95th,96th,97th,98th,99th,100th,101st,102nd,103rd,104th,105th,106th,107th,108th,109th,110th,111th,112th,113th,114th,115th,116th,117th,118th,119th,120th",
        ),
        (
            "cardinalText",
            "One,Two,Three,Four,Five,Six,Seven,Eight,Nine,Ten,Eleven,Twelve,Thirteen,Fourteen,Fifteen,Sixteen,Seventeen,Eighteen,Nineteen,Twenty,Twenty-one,Twenty-two,Twenty-three,Twenty-four,Twenty-five,Twenty-six,Twenty-seven,Twenty-eight,Twenty-nine,Thirty,Thirty-one,Thirty-two,Thirty-three,Thirty-four,Thirty-five,Thirty-six,Thirty-seven,Thirty-eight,Thirty-nine,Forty,Forty-one,Forty-two,Forty-three,Forty-four,Forty-five,Forty-six,Forty-seven,Forty-eight,Forty-nine,Fifty,Fifty-one,Fifty-two,Fifty-three,Fifty-four,Fifty-five,Fifty-six,Fifty-seven,Fifty-eight,Fifty-nine,Sixty,Sixty-one,Sixty-two,Sixty-three,Sixty-four,Sixty-five,Sixty-six,Sixty-seven,Sixty-eight,Sixty-nine,Seventy,Seventy-one,Seventy-two,Seventy-three,Seventy-four,Seventy-five,Seventy-six,Seventy-seven,Seventy-eight,Seventy-nine,Eighty,Eighty-one,Eighty-two,Eighty-three,Eighty-four,Eighty-five,Eighty-six,Eighty-seven,Eighty-eight,Eighty-nine,Ninety,Ninety-one,Ninety-two,Ninety-three,Ninety-four,Ninety-five,Ninety-six,Ninety-seven,Ninety-eight,Ninety-nine,One hundred,One hundred one,One hundred two,One hundred three,One hundred four,One hundred five,One hundred six,One hundred seven,One hundred eight,One hundred nine,One hundred ten,One hundred eleven,One hundred twelve,One hundred thirteen,One hundred fourteen,One hundred fifteen,One hundred sixteen,One hundred seventeen,One hundred eighteen,One hundred nineteen,One hundred twenty",
        ),
        (
            "ordinalText",
            "First,Second,Third,Fourth,Fifth,Sixth,Seventh,Eighth,Ninth,Tenth,Eleventh,Twelfth,Thirteenth,Fourteenth,Fifteenth,Sixteenth,Seventeenth,Eighteenth,Nineteenth,Twentieth,Twenty-first,Twenty-second,Twenty-third,Twenty-fourth,Twenty-fifth,Twenty-sixth,Twenty-seventh,Twenty-eighth,Twenty-ninth,Thirtieth,Thirty-first,Thirty-second,Thirty-third,Thirty-fourth,Thirty-fifth,Thirty-sixth,Thirty-seventh,Thirty-eighth,Thirty-ninth,Fortieth,Forty-first,Forty-second,Forty-third,Forty-fourth,Forty-fifth,Forty-sixth,Forty-seventh,Forty-eighth,Forty-ninth,Fiftieth,Fifty-first,Fifty-second,Fifty-third,Fifty-fourth,Fifty-fifth,Fifty-sixth,Fifty-seventh,Fifty-eighth,Fifty-ninth,Sixtieth,Sixty-first,Sixty-second,Sixty-third,Sixty-fourth,Sixty-fifth,Sixty-sixth,Sixty-seventh,Sixty-eighth,Sixty-ninth,Seventieth,Seventy-first,Seventy-second,Seventy-third,Seventy-fourth,Seventy-fifth,Seventy-sixth,Seventy-seventh,Seventy-eighth,Seventy-ninth,Eightieth,Eighty-first,Eighty-second,Eighty-third,Eighty-fourth,Eighty-fifth,Eighty-sixth,Eighty-seventh,Eighty-eighth,Eighty-ninth,Ninetieth,Ninety-first,Ninety-second,Ninety-third,Ninety-fourth,Ninety-fifth,Ninety-sixth,Ninety-seventh,Ninety-eighth,Ninety-ninth,One hundredth,One hundred first,One hundred second,One hundred third,One hundred fourth,One hundred fifth,One hundred sixth,One hundred seventh,One hundred eighth,One hundred ninth,One hundred tenth,One hundred eleventh,One hundred twelfth,One hundred thirteenth,One hundred fourteenth,One hundred fifteenth,One hundred sixteenth,One hundred seventeenth,One hundred eighteenth,One hundred nineteenth,One hundred twentieth",
        ),
        (
            "decimalZero",
            "01,02,03,04,05,06,07,08,09,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95,96,97,98,99,100,101,102,103,104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120",
        ),
        (
            "decimalEnclosedCircle",
            "①,②,③,④,⑤,⑥,⑦,⑧,⑨,⑩,⑪,⑫,⑬,⑭,⑮,⑯,⑰,⑱,⑲,⑳,㉑,㉒,㉓,㉔,㉕,㉖,㉗,㉘,㉙,㉚,㉛,㉜,㉝,㉞,㉟,㊱,㊲,㊳,㊴,㊵,㊶,㊷,㊸,㊹,㊺,㊻,㊼,㊽,㊾,㊿,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95,96,97,98,99,100,101,102,103,104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120",
        ),
        (
            "decimalEnclosedCircleChinese",
            "①,②,③,④,⑤,⑥,⑦,⑧,⑨,⑩,⑪,⑫,⑬,⑭,⑮,⑯,⑰,⑱,⑲,⑳,㉑,㉒,㉓,㉔,㉕,㉖,㉗,㉘,㉙,㉚,㉛,㉜,㉝,㉞,㉟,㊱,㊲,㊳,㊴,㊵,㊶,㊷,㊸,㊹,㊺,㊻,㊼,㊽,㊾,㊿,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95,96,97,98,99,100,101,102,103,104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120",
        ),
        (
            "chineseCounting",
            "一,二,三,四,五,六,七,八,九,十,十一,十二,十三,十四,十五,十六,十七,十八,十九,二十,二十一,二十二,二十三,二十四,二十五,二十六,二十七,二十八,二十九,三十,三十一,三十二,三十三,三十四,三十五,三十六,三十七,三十八,三十九,四十,四十一,四十二,四十三,四十四,四十五,四十六,四十七,四十八,四十九,五十,五十一,五十二,五十三,五十四,五十五,五十六,五十七,五十八,五十九,六十,六十一,六十二,六十三,六十四,六十五,六十六,六十七,六十八,六十九,七十,七十一,七十二,七十三,七十四,七十五,七十六,七十七,七十八,七十九,八十,八十一,八十二,八十三,八十四,八十五,八十六,八十七,八十八,八十九,九十,九十一,九十二,九十三,九十四,九十五,九十六,九十七,九十八,九十九,一百,一百零一,一百零二,一百零三,一百零四,一百零五,一百零六,一百零七,一百零八,一百零九,一百一十,一百一十一,一百一十二,一百一十三,一百一十四,一百一十五,一百一十六,一百一十七,一百一十八,一百一十九,一百二十",
        ),
        (
            "chineseCountingThousand",
            "一,二,三,四,五,六,七,八,九,十,十一,十二,十三,十四,十五,十六,十七,十八,十九,二十,二十一,二十二,二十三,二十四,二十五,二十六,二十七,二十八,二十九,三十,三十一,三十二,三十三,三十四,三十五,三十六,三十七,三十八,三十九,四十,四十一,四十二,四十三,四十四,四十五,四十六,四十七,四十八,四十九,五十,五十一,五十二,五十三,五十四,五十五,五十六,五十七,五十八,五十九,六十,六十一,六十二,六十三,六十四,六十五,六十六,六十七,六十八,六十九,七十,七十一,七十二,七十三,七十四,七十五,七十六,七十七,七十八,七十九,八十,八十一,八十二,八十三,八十四,八十五,八十六,八十七,八十八,八十九,九十,九十一,九十二,九十三,九十四,九十五,九十六,九十七,九十八,九十九,一百,一百零一,一百零二,一百零三,一百零四,一百零五,一百零六,一百零七,一百零八,一百零九,一百一十,一百一十一,一百一十二,一百一十三,一百一十四,一百一十五,一百一十六,一百一十七,一百一十八,一百一十九,一百二十",
        ),
        (
            "chineseLegalSimplified",
            "壹,贰,叁,肆,伍,陆,柒,捌,玖,壹拾,壹拾壹,壹拾贰,壹拾叁,壹拾肆,壹拾伍,壹拾陆,壹拾柒,壹拾捌,壹拾玖,贰拾,贰拾壹,贰拾贰,贰拾叁,贰拾肆,贰拾伍,贰拾陆,贰拾柒,贰拾捌,贰拾玖,叁拾,叁拾壹,叁拾贰,叁拾叁,叁拾肆,叁拾伍,叁拾陆,叁拾柒,叁拾捌,叁拾玖,肆拾,肆拾壹,肆拾贰,肆拾叁,肆拾肆,肆拾伍,肆拾陆,肆拾柒,肆拾捌,肆拾玖,伍拾,伍拾壹,伍拾贰,伍拾叁,伍拾肆,伍拾伍,伍拾陆,伍拾柒,伍拾捌,伍拾玖,陆拾,陆拾壹,陆拾贰,陆拾叁,陆拾肆,陆拾伍,陆拾陆,陆拾柒,陆拾捌,陆拾玖,柒拾,柒拾壹,柒拾贰,柒拾叁,柒拾肆,柒拾伍,柒拾陆,柒拾柒,柒拾捌,柒拾玖,捌拾,捌拾壹,捌拾贰,捌拾叁,捌拾肆,捌拾伍,捌拾陆,捌拾柒,捌拾捌,捌拾玖,玖拾,玖拾壹,玖拾贰,玖拾叁,玖拾肆,玖拾伍,玖拾陆,玖拾柒,玖拾捌,玖拾玖,壹佰,壹佰零壹,壹佰零贰,壹佰零叁,壹佰零肆,壹佰零伍,壹佰零陆,壹佰零柒,壹佰零捌,壹佰零玖,壹佰壹拾,壹佰壹拾壹,壹佰壹拾贰,壹佰壹拾叁,壹佰壹拾肆,壹佰壹拾伍,壹佰壹拾陆,壹佰壹拾柒,壹佰壹拾捌,壹佰壹拾玖,壹佰贰拾",
        ),
        (
            "ideographTraditional",
            "甲,乙,丙,丁,戊,己,庚,辛,壬,癸,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95,96,97,98,99,100,101,102,103,104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120",
        ),
        (
            "ideographZodiac",
            "子,丑,寅,卯,辰,巳,午,未,申,酉,戌,亥,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95,96,97,98,99,100,101,102,103,104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120",
        ),
        (
            "ideographDigital",
            "一,二,三,四,五,六,七,八,九,十,十一,十二,十三,十四,十五,十六,十七,十八,十九,二十,二十一,二十二,二十三,二十四,二十五,二十六,二十七,二十八,二十九,三十,三十一,三十二,三十三,三十四,三十五,三十六,三十七,三十八,三十九,四十,四十一,四十二,四十三,四十四,四十五,四十六,四十七,四十八,四十九,五十,五十一,五十二,五十三,五十四,五十五,五十六,五十七,五十八,五十九,六十,六十一,六十二,六十三,六十四,六十五,六十六,六十七,六十八,六十九,七十,七十一,七十二,七十三,七十四,七十五,七十六,七十七,七十八,七十九,八十,八十一,八十二,八十三,八十四,八十五,八十六,八十七,八十八,八十九,九十,九十一,九十二,九十三,九十四,九十五,九十六,九十七,九十八,九十九,一百,一百零一,一百零二,一百零三,一百零四,一百零五,一百零六,一百零七,一百零八,一百零九,一百一十,一百一十一,一百一十二,一百一十三,一百一十四,一百一十五,一百一十六,一百一十七,一百一十八,一百一十九,一百二十",
        ),
        (
            "japaneseCounting",
            "一,二,三,四,五,六,七,八,九,十,十一,十二,十三,十四,十五,十六,十七,十八,十九,二十,二十一,二十二,二十三,二十四,二十五,二十六,二十七,二十八,二十九,三十,三十一,三十二,三十三,三十四,三十五,三十六,三十七,三十八,三十九,四十,四十一,四十二,四十三,四十四,四十五,四十六,四十七,四十八,四十九,五十,五十一,五十二,五十三,五十四,五十五,五十六,五十七,五十八,五十九,六十,六十一,六十二,六十三,六十四,六十五,六十六,六十七,六十八,六十九,七十,七十一,七十二,七十三,七十四,七十五,七十六,七十七,七十八,七十九,八十,八十一,八十二,八十三,八十四,八十五,八十六,八十七,八十八,八十九,九十,九十一,九十二,九十三,九十四,九十五,九十六,九十七,九十八,九十九,一百,一百零一,一百零二,一百零三,一百零四,一百零五,一百零六,一百零七,一百零八,一百零九,一百一十,一百一十一,一百一十二,一百一十三,一百一十四,一百一十五,一百一十六,一百一十七,一百一十八,一百一十九,一百二十",
        ),
        (
            "taiwaneseCounting",
            "一,二,三,四,五,六,七,八,九,十,十一,十二,十三,十四,十五,十六,十七,十八,十九,二十,二十一,二十二,二十三,二十四,二十五,二十六,二十七,二十八,二十九,三十,三十一,三十二,三十三,三十四,三十五,三十六,三十七,三十八,三十九,四十,四十一,四十二,四十三,四十四,四十五,四十六,四十七,四十八,四十九,五十,五十一,五十二,五十三,五十四,五十五,五十六,五十七,五十八,五十九,六十,六十一,六十二,六十三,六十四,六十五,六十六,六十七,六十八,六十九,七十,七十一,七十二,七十三,七十四,七十五,七十六,七十七,七十八,七十九,八十,八十一,八十二,八十三,八十四,八十五,八十六,八十七,八十八,八十九,九十,九十一,九十二,九十三,九十四,九十五,九十六,九十七,九十八,九十九,一百,一百零一,一百零二,一百零三,一百零四,一百零五,一百零六,一百零七,一百零八,一百零九,一百一十,一百一十一,一百一十二,一百一十三,一百一十四,一百一十五,一百一十六,一百一十七,一百一十八,一百一十九,一百二十",
        ),
        (
            "ideographEnclosedCircle",
            "①,②,③,④,⑤,⑥,⑦,⑧,⑨,⑩,⑪,⑫,⑬,⑭,⑮,⑯,⑰,⑱,⑲,⑳,㉑,㉒,㉓,㉔,㉕,㉖,㉗,㉘,㉙,㉚,㉛,㉜,㉝,㉞,㉟,㊱,㊲,㊳,㊴,㊵,㊶,㊷,㊸,㊹,㊺,㊻,㊼,㊽,㊾,㊿,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95,96,97,98,99,100,101,102,103,104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120",
        ),
        (
            "ideographLegalTraditional",
            "壹,貳,參,肆,伍,陸,柒,捌,玖,壹拾,壹拾壹,壹拾貳,壹拾參,壹拾肆,壹拾伍,壹拾陸,壹拾柒,壹拾捌,壹拾玖,貳拾,貳拾壹,貳拾貳,貳拾參,貳拾肆,貳拾伍,貳拾陸,貳拾柒,貳拾捌,貳拾玖,參拾,參拾壹,參拾貳,參拾參,參拾肆,參拾伍,參拾陸,參拾柒,參拾捌,參拾玖,肆拾,肆拾壹,肆拾貳,肆拾參,肆拾肆,肆拾伍,肆拾陸,肆拾柒,肆拾捌,肆拾玖,伍拾,伍拾壹,伍拾貳,伍拾參,伍拾肆,伍拾伍,伍拾陸,伍拾柒,伍拾捌,伍拾玖,陸拾,陸拾壹,陸拾貳,陸拾參,陸拾肆,陸拾伍,陸拾陸,陸拾柒,陸拾捌,陸拾玖,柒拾,柒拾壹,柒拾貳,柒拾參,柒拾肆,柒拾伍,柒拾陸,柒拾柒,柒拾捌,柒拾玖,捌拾,捌拾壹,捌拾貳,捌拾參,捌拾肆,捌拾伍,捌拾陸,捌拾柒,捌拾捌,捌拾玖,玖拾,玖拾壹,玖拾貳,玖拾參,玖拾肆,玖拾伍,玖拾陸,玖拾柒,玖拾捌,玖拾玖,壹佰,壹佰零壹,壹佰零貳,壹佰零參,壹佰零肆,壹佰零伍,壹佰零陸,壹佰零柒,壹佰零捌,壹佰零玖,壹佰壹拾,壹佰壹拾壹,壹佰壹拾貳,壹佰壹拾參,壹佰壹拾肆,壹佰壹拾伍,壹佰壹拾陸,壹佰壹拾柒,壹佰壹拾捌,壹佰壹拾玖,壹佰貳拾",
        ),
        (
            "taiwaneseCountingThousand",
            "一,二,三,四,五,六,七,八,九,十,十一,十二,十三,十四,十五,十六,十七,十八,十九,二十,二十一,二十二,二十三,二十四,二十五,二十六,二十七,二十八,二十九,三十,三十一,三十二,三十三,三十四,三十五,三十六,三十七,三十八,三十九,四十,四十一,四十二,四十三,四十四,四十五,四十六,四十七,四十八,四十九,五十,五十一,五十二,五十三,五十四,五十五,五十六,五十七,五十八,五十九,六十,六十一,六十二,六十三,六十四,六十五,六十六,六十七,六十八,六十九,七十,七十一,七十二,七十三,七十四,七十五,七十六,七十七,七十八,七十九,八十,八十一,八十二,八十三,八十四,八十五,八十六,八十七,八十八,八十九,九十,九十一,九十二,九十三,九十四,九十五,九十六,九十七,九十八,九十九,一百,一百零一,一百零二,一百零三,一百零四,一百零五,一百零六,一百零七,一百零八,一百零九,一百一十,一百一十一,一百一十二,一百一十三,一百一十四,一百一十五,一百一十六,一百一十七,一百一十八,一百一十九,一百二十",
        ),
    ];

    #[test]
    fn matches_libreoffice_for_1_to_120() {
        for (fmt, expected) in GOLDEN {
            for (i, want) in expected.split(',').enumerate() {
                let n = i as i32 + 1;
                assert_eq!(format(n, fmt).as_deref(), Some(want), "{fmt} 第 {n} 项");
            }
        }
    }

    /// LibreOffice 画成普通数字的三种格式，按规范：全角数字；⑴…⒇、⒈…⒛ 到 20 为止。
    #[test]
    fn formats_libreoffice_drops_follow_the_spec() {
        assert_eq!(format(120, "decimalFullWidth").unwrap(), "１２０");
        assert_eq!(format(1, "decimalEnclosedParen").unwrap(), "⑴");
        assert_eq!(format(20, "decimalEnclosedParen").unwrap(), "⒇");
        assert_eq!(format(21, "decimalEnclosedParen").unwrap(), "21");
        assert_eq!(format(1, "decimalEnclosedFullstop").unwrap(), "⒈");
        assert_eq!(format(20, "decimalEnclosedFullstop").unwrap(), "⒛");
        assert_eq!(format(21, "decimalEnclosedFullstop").unwrap(), "21");
    }

    #[test]
    fn larger_chinese_numbers_read_zeros_once() {
        assert_eq!(format(1001, "chineseCounting").unwrap(), "一千零一");
        assert_eq!(format(1010, "chineseCounting").unwrap(), "一千零一十");
        assert_eq!(format(10001, "chineseCounting").unwrap(), "一万零一");
        assert_eq!(format(100000, "chineseCounting").unwrap(), "十万");
        assert_eq!(
            format(2020, "chineseLegalSimplified").unwrap(),
            "贰仟零贰拾"
        );
    }

    #[test]
    fn unknown_formats_are_reported() {
        assert_eq!(format(3, "hebrew1"), None);
        assert_eq!(format(3, "none").unwrap(), "");
    }
}
