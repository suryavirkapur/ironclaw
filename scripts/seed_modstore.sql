-- ============================================================================
-- ModStore seed script — online retailer selling video game mods
-- Wipes the public schema and seeds ~200MB of realistic demo data.
-- Run:  psql "$DATABASE_URL" -f scripts/seed_modstore.sql
-- ============================================================================
\set ON_ERROR_STOP on
\timing on
SET synchronous_commit = off;
SET work_mem = '256MB';

\echo '=== Wiping existing schema ==='
DROP SCHEMA public CASCADE;
CREATE SCHEMA public;

-- ============================================================================
-- DDL
-- ============================================================================
\echo '=== Creating tables ==='

CREATE TABLE countries (
    code             text PRIMARY KEY,          -- ISO-3166 alpha-2
    name             text NOT NULL,
    currency         text NOT NULL,
    fx_rate          numeric(14,4) NOT NULL,    -- local currency per 1 USD
    vat_rate         numeric(5,4)  NOT NULL,
    locale_group     text NOT NULL,             -- name-pool key
    player_weight    numeric(8,3) NOT NULL,     -- relative player-base size
    buyer_propensity numeric(5,3) NOT NULL,     -- >1 buys more, <1 buys less
    cities           text[] NOT NULL            -- for "City, CC" region labels
);

CREATE TABLE games (
    id           bigint PRIMARY KEY,
    name         text NOT NULL,
    short_name   text NOT NULL,
    developer    text NOT NULL,
    publisher    text NOT NULL,
    release_date date NOT NULL,
    genre        text NOT NULL,
    mod_weight   numeric(6,2) NOT NULL          -- share of the mod catalog
);

CREATE TABLE categories (
    id   bigint PRIMARY KEY,
    name text NOT NULL UNIQUE
);

CREATE TABLE creators (
    id            bigint PRIMARY KEY,
    handle        text NOT NULL UNIQUE,
    display_name  text NOT NULL,
    email         text NOT NULL UNIQUE,
    country_code  text NOT NULL,
    payout_method text NOT NULL,                -- paypal | bank | crypto
    is_verified   boolean NOT NULL,
    joined_at     timestamptz NOT NULL
);

CREATE TABLE users (
    id            bigint PRIMARY KEY,
    username      text NOT NULL,
    email         text NOT NULL,
    password_hash text NOT NULL,
    first_name    text NOT NULL,
    last_name     text NOT NULL,
    display_name  text NOT NULL,
    country_code  text NOT NULL,
    region        text NOT NULL,                -- e.g. 'Amsterdam, NL'
    signup_ip     inet NOT NULL,
    is_verified   boolean NOT NULL,
    created_at    timestamptz NOT NULL,
    last_login_at timestamptz
);

CREATE TABLE products (
    id           bigint PRIMARY KEY,
    game_id      bigint NOT NULL,
    category_id  bigint NOT NULL,
    creator_id   bigint NOT NULL,
    name         text NOT NULL,
    slug         text NOT NULL,
    description  text NOT NULL,
    price_usd    numeric(10,2) NOT NULL,
    version      text NOT NULL,
    file_size_mb numeric(10,1) NOT NULL,
    is_active    boolean NOT NULL,
    created_at   timestamptz NOT NULL
);

CREATE TABLE orders (
    id             bigint PRIMARY KEY,
    user_id        bigint NOT NULL,
    status         text NOT NULL,               -- completed | refunded | failed | pending
    payment_method text NOT NULL,               -- stripe | paypal | crypto
    currency       text NOT NULL,
    subtotal       numeric(12,2) NOT NULL,
    tax            numeric(12,2) NOT NULL,
    total          numeric(12,2) NOT NULL,
    ip_address     inet NOT NULL,
    region         text NOT NULL,
    created_at     timestamptz NOT NULL
);

CREATE TABLE order_items (
    id           bigint PRIMARY KEY,
    order_id     bigint NOT NULL,
    product_id   bigint NOT NULL,
    unit_price   numeric(12,2) NOT NULL,        -- snapshot in order currency
    quantity     int NOT NULL,
    discount_pct int NOT NULL,
    final_price  numeric(12,2) NOT NULL
);

CREATE TABLE payments (
    id           bigint PRIMARY KEY,
    order_id     bigint NOT NULL,
    provider     text NOT NULL,                 -- stripe | paypal | crypto
    provider_ref text NOT NULL,
    status       text NOT NULL,                 -- succeeded | refunded | failed | processing
    card_brand   text,
    card_last4   text,
    crypto_coin  text,                          -- BTC | ETH | USDT | LTC
    amount       numeric(12,2) NOT NULL,
    currency     text NOT NULL,
    created_at   timestamptz NOT NULL
);

CREATE TABLE reviews (
    id            bigint PRIMARY KEY,
    user_id       bigint NOT NULL,
    product_id    bigint NOT NULL,
    order_id      bigint NOT NULL,
    rating        int NOT NULL,
    title         text NOT NULL,
    body          text NOT NULL,
    helpful_count int NOT NULL,
    created_at    timestamptz NOT NULL
);

CREATE TABLE wishlists (
    id         bigint PRIMARY KEY,
    user_id    bigint NOT NULL,
    product_id bigint NOT NULL,
    added_at   timestamptz NOT NULL
);

CREATE TABLE download_events (
    id            bigint PRIMARY KEY,
    order_item_id bigint NOT NULL,
    user_id       bigint NOT NULL,
    ip_address    inet NOT NULL,
    region        text NOT NULL,
    user_agent    text NOT NULL,
    downloaded_at timestamptz NOT NULL
);

-- ============================================================================
-- Reference data: locale name pools (temp, generation only)
-- ============================================================================
\echo '=== Loading locale name pools ==='

CREATE TEMP TABLE name_pools (
    locale_group text PRIMARY KEY,
    first_names  text[] NOT NULL,
    last_names   text[] NOT NULL,
    domains      text[] NOT NULL,
    ip_prefixes  text[] NOT NULL
);

INSERT INTO name_pools VALUES
('en_us',
 ARRAY['James','John','Robert','Michael','William','David','Richard','Joseph','Thomas','Christopher','Daniel','Matthew','Anthony','Mark','Kevin','Jason','Brian','Ryan','Tyler','Brandon','Justin','Austin','Jacob','Ethan','Noah','Logan','Mason','Hunter','Mary','Patricia','Jennifer','Linda','Elizabeth','Jessica','Sarah','Ashley','Emily','Madison','Hannah','Samantha','Alexis','Rachel','Kayla','Megan','Lauren','Olivia','Ava','Sophia','Isabella'],
 ARRAY['Smith','Johnson','Williams','Brown','Jones','Garcia','Miller','Davis','Rodriguez','Martinez','Wilson','Anderson','Taylor','Thomas','Moore','Jackson','Martin','Lee','Thompson','White','Harris','Clark','Lewis','Walker','Hall','Young','King','Wright','Scott','Green','Baker','Adams','Nelson','Carter','Mitchell','Turner','Phillips','Campbell','Parker','Evans'],
 ARRAY['gmail.com','outlook.com','yahoo.com','hotmail.com','icloud.com','proton.me','aol.com'],
 ARRAY['73','98','24','67','75','172','96','108','66','76']),
('en_gb',
 ARRAY['Oliver','George','Harry','Jack','Jacob','Noah','Charlie','Leo','Oscar','Alfie','Joshua','Archie','Henry','Thomas','Freddie','Amelia','Olivia','Isla','Ava','Grace','Freya','Ivy','Sophia','Emily','Jessica','Ruby','Ella','Chloe','Poppy','Evie'],
 ARRAY['Smith','Jones','Taylor','Brown','Williams','Wilson','Johnson','Davies','Robinson','Wright','Thompson','Evans','Walker','White','Roberts','Green','Hall','Wood','Jackson','Clarke','Patel','Lewis','Hughes','Edwards','Morgan','Bell','Murphy','Cox','Bailey','Richardson'],
 ARRAY['gmail.com','outlook.com','hotmail.co.uk','yahoo.co.uk','icloud.com','btinternet.com'],
 ARRAY['82','86','92','31','78','2','51','77']),
('pt_br',
 ARRAY['Miguel','Arthur','Heitor','Theo','Davi','Gabriel','Bernardo','Samuel','Joao','Pedro','Lucas','Matheus','Rafael','Guilherme','Enzo','Thiago','Felipe','Bruno','Vinicius','Gustavo','Leonardo','Rodrigo','Marcos','Caio','Alice','Helena','Laura','Maria','Valentina','Sophia','Isabella','Manuela','Julia','Ana','Beatriz','Mariana','Gabriela','Fernanda','Larissa','Camila','Amanda','Bruna','Juliana','Patricia','Leticia'],
 ARRAY['Silva','Santos','Oliveira','Souza','Rodrigues','Ferreira','Alves','Pereira','Lima','Gomes','Costa','Ribeiro','Martins','Carvalho','Almeida','Lopes','Soares','Fernandes','Vieira','Barbosa','Rocha','Dias','Nascimento','Andrade','Moreira','Nunes','Marques','Machado','Mendes','Freitas','Cardoso','Ramos','Teixeira','Correia','Araujo'],
 ARRAY['gmail.com','hotmail.com','outlook.com','uol.com.br','terra.com.br','bol.com.br','yahoo.com.br'],
 ARRAY['177','179','186','187','189','191','201','200']),
('pt_pt',
 ARRAY['Joao','Francisco','Rodrigo','Martim','Santiago','Afonso','Tomas','Duarte','Miguel','Gabriel','Maria','Leonor','Matilde','Beatriz','Carolina','Mariana','Ana','Ines','Margarida','Sofia'],
 ARRAY['Silva','Santos','Ferreira','Pereira','Oliveira','Costa','Rodrigues','Martins','Sousa','Fernandes','Goncalves','Gomes','Lopes','Marques','Alves','Almeida','Ribeiro','Pinto','Carvalho','Teixeira'],
 ARRAY['gmail.com','sapo.pt','outlook.com','hotmail.com','mail.pt'],
 ARRAY['85','89','95','188','2','31']),
('es_latam',
 ARRAY['Santiago','Mateo','Sebastian','Diego','Alejandro','Juan','Carlos','Luis','Miguel','Andres','Fernando','Jorge','Ricardo','Eduardo','Emiliano','Leonardo','Daniel','Gabriel','Angel','Jesus','Sofia','Valentina','Isabella','Camila','Valeria','Maria','Fernanda','Daniela','Gabriela','Alejandra','Carolina','Lucia','Ximena','Mariana','Andrea','Paula','Natalia','Paola','Jose','Manuel'],
 ARRAY['Garcia','Rodriguez','Gonzalez','Hernandez','Lopez','Martinez','Perez','Sanchez','Ramirez','Torres','Flores','Rivera','Gomez','Diaz','Cruz','Morales','Reyes','Gutierrez','Ortiz','Chavez','Ruiz','Alvarez','Mendoza','Vargas','Castillo','Jimenez','Moreno','Rojas','Medina','Herrera','Aguilar','Vega','Castro','Romero','Navarro'],
 ARRAY['gmail.com','hotmail.com','outlook.com','yahoo.com','live.com','prodigy.net.mx'],
 ARRAY['181','186','187','190','191','200','201','189']),
('es_es',
 ARRAY['Hugo','Martin','Lucas','Mateo','Leo','Daniel','Alejandro','Pablo','Alvaro','Adrian','Enzo','Mario','Lucia','Sofia','Martina','Maria','Julia','Paula','Valeria','Emma','Daniela','Carla','Alba','Sara'],
 ARRAY['Garcia','Fernandez','Gonzalez','Rodriguez','Lopez','Martinez','Sanchez','Perez','Gomez','Martin','Jimenez','Ruiz','Hernandez','Diaz','Moreno','Munoz','Alvarez','Romero','Alonso','Gutierrez','Navarro','Torres','Dominguez','Vazquez','Ramos','Gil','Serrano','Blanco','Molina','Castro'],
 ARRAY['gmail.com','hotmail.es','outlook.es','yahoo.es','telefonica.net'],
 ARRAY['83','88','95','31','2','176']),
('de',
 ARRAY['Lukas','Maximilian','Felix','Paul','Jonas','Leon','Luca','Finn','Elias','Niklas','Tim','Julian','Moritz','Philipp','Sebastian','Tobias','Jan','Florian','Emma','Mia','Hannah','Emilia','Lina','Marie','Lena','Lea','Anna','Laura','Sophie','Johanna','Clara','Leni','Lara','Maja'],
 ARRAY['Mueller','Schmidt','Schneider','Fischer','Weber','Meyer','Wagner','Becker','Hoffmann','Schulz','Schaefer','Koch','Bauer','Richter','Klein','Wolf','Schroeder','Neumann','Schwarz','Zimmermann','Braun','Krueger','Hofmann','Hartmann','Lange','Schmitt','Werner','Krause','Meier','Lehmann','Huber','Mayer','Walter','Kraus','Jung'],
 ARRAY['gmail.com','gmx.de','web.de','outlook.de','t-online.de','freenet.de'],
 ARRAY['77','78','79','84','85','87','92','95','2','31']),
('fr',
 ARRAY['Gabriel','Leo','Raphael','Louis','Arthur','Jules','Adam','Lucas','Hugo','Theo','Nathan','Ethan','Tom','Noah','Emma','Jade','Louise','Alice','Chloe','Lea','Manon','Anna','Camille','Ines','Sarah','Juliette','Zoe','Lina','Rose','Eva'],
 ARRAY['Martin','Bernard','Dubois','Thomas','Robert','Richard','Petit','Durand','Leroy','Moreau','Simon','Laurent','Lefevre','Michel','David','Bertrand','Roux','Vincent','Fournier','Morel','Girard','Andre','Lefebvre','Mercier','Dupont','Lambert','Bonnet','Fontaine','Rousseau','Chevalier','Gauthier','Perrin','Robin','Clement','Morin'],
 ARRAY['gmail.com','orange.fr','free.fr','hotmail.fr','outlook.fr','laposte.net','sfr.fr'],
 ARRAY['82','90','92','109','176','2','31']),
('it',
 ARRAY['Leonardo','Francesco','Alessandro','Lorenzo','Mattia','Andrea','Gabriele','Riccardo','Tommaso','Edoardo','Marco','Giuseppe','Antonio','Giovanni','Sofia','Aurora','Giulia','Ginevra','Beatrice','Alice','Emma','Giorgia','Vittoria','Matilde','Anna','Chiara','Francesca','Martina','Sara'],
 ARRAY['Rossi','Russo','Ferrari','Esposito','Bianchi','Romano','Colombo','Ricci','Marino','Greco','Bruno','Gallo','Conti','DeLuca','Mancini','Costa','Giordano','Rizzo','Lombardi','Moretti','Barbieri','Fontana','Santoro','Mariani','Rinaldi','Caruso','Ferrara','Galli','Martini','Leone','Vitale','Palmieri','Serra','DeAngelis','Marchetti'],
 ARRAY['gmail.com','libero.it','virgilio.it','hotmail.it','outlook.it','tiscali.it'],
 ARRAY['79','82','87','93','95','151','2','31']),
('nl',
 ARRAY['Daan','Sem','Lucas','Milan','Levi','Luuk','Mees','Finn','Bram','Thijs','Jesse','Sven','Lars','Stijn','Emma','Tess','Julia','Sophie','Anna','Mila','Sara','Eva','Noor','Lotte','Saar','Lieke','Fleur','Isa','Nina','Liv'],
 ARRAY['deJong','Jansen','deVries','vandenBerg','vanDijk','Bakker','Janssen','Visser','Smit','Meijer','deBoer','Mulder','deGroot','Bos','Vos','Peters','Hendriks','vanLeeuwen','Dekker','Brouwer','deWit','Dijkstra','Smits','deGraaf','vanderMeer','vanderLinden','Kok','Jacobs','vanVliet','Willems'],
 ARRAY['gmail.com','hotmail.nl','ziggo.nl','kpnmail.nl','outlook.com','live.nl'],
 ARRAY['77','82','83','84','85','145','2','31']),
('pl',
 ARRAY['Jakub','Jan','Antoni','Aleksander','Franciszek','Leon','Mikolaj','Stanislaw','Wojciech','Adam','Kacper','Szymon','Filip','Zuzanna','Julia','Maja','Zofia','Hanna','Alicja','Maria','Amelia','Oliwia','Lena','Wiktoria','Kornelia','Natalia','Klaudia'],
 ARRAY['Nowak','Kowalski','Wisniewski','Wojcik','Kowalczyk','Kaminski','Lewandowski','Zielinski','Szymanski','Wozniak','Dabrowski','Kozlowski','Jankowski','Mazur','Kwiatkowski','Krawczyk','Kaczmarek','Piotrowski','Grabowski','Nowakowski','Pawlowski','Michalski','Adamczyk','Dudek','Zajac','Wieczorek','Jablonski','Krol','Majewski','Olszewski','Wrobel','Gorski','Rutkowski','Sikora','Baran'],
 ARRAY['gmail.com','wp.pl','onet.pl','interia.pl','o2.pl','poczta.fm'],
 ARRAY['77','79','83','89','91','188','31','46']),
('ru',
 ARRAY['Alexander','Dmitry','Maxim','Sergey','Andrey','Alexey','Artyom','Ilya','Kirill','Mikhail','Nikita','Ivan','Daniil','Egor','Vladimir','Roman','Pavel','Denis','Sofia','Maria','Anna','Victoria','Anastasia','Daria','Alisa','Polina','Elizaveta','Ekaterina','Ksenia','Olga','Tatiana','Natalia','Irina','Svetlana'],
 ARRAY['Ivanov','Smirnov','Kuznetsov','Popov','Vasilyev','Petrov','Sokolov','Mikhailov','Novikov','Fedorov','Morozov','Volkov','Alekseev','Lebedev','Semenov','Egorov','Pavlov','Kozlov','Stepanov','Nikolaev','Orlov','Andreev','Makarov','Nikitin','Zakharov','Zaitsev','Solovyov','Borisov','Yakovlev','Grigoriev','Romanov','Vorobyov','Sergeev','Tarasov','Belov'],
 ARRAY['gmail.com','yandex.ru','mail.ru','bk.ru','inbox.ru','rambler.ru'],
 ARRAY['77','79','85','90','91','92','93','95','178','188','46','5']),
('uk',
 ARRAY['Oleksandr','Andriy','Dmytro','Maksym','Ivan','Taras','Bohdan','Mykola','Vasyl','Petro','Yuriy','Vladyslav','Nazar','Roman','Anastasiya','Olena','Kateryna','Mariya','Natalia','Yulia','Viktoria','Daryna','Sofia','Khrystyna','Solomiia','Iryna'],
 ARRAY['Shevchenko','Kovalenko','Bondarenko','Tkachenko','Boyko','Kravchenko','Koval','Melnyk','Polischuk','Savchenko','Moroz','Marchenko','Rudenko','Lysenko','Pavlenko','Khomenko','Vasilenko','Kovalchuk','Gonchar','Petrenko','Savchuk','Zakharchenko','Ponomarenko','Levchenko','Tkachuk'],
 ARRAY['gmail.com','ukr.net','i.ua','meta.ua','outlook.com'],
 ARRAY['91','93','95','178','188','46','77','31']),
('jp',
 ARRAY['Haruto','Sota','Yuto','Aoi','Riku','Haruki','Kaito','Ren','Itsuki','Hinata','Asahi','Takumi','Daiki','Shota','Kenta','Yuki','Sho','Kazuki','Yui','Himari','Mei','Hina','Sakura','Rin','Ichika','Koharu','Mio','Yuna','Airi','Nanami','Misaki','Riko','Ayaka','Saki','Mai'],
 ARRAY['Sato','Suzuki','Takahashi','Tanaka','Watanabe','Yamamoto','Nakamura','Kobayashi','Kato','Yoshida','Yamada','Sasaki','Yamaguchi','Saito','Matsumoto','Inoue','Kimura','Hayashi','Shimizu','Yamazaki','Mori','Abe','Ikeda','Hashimoto','Ishikawa','Nakajima','Maeda','Fujita','Ogawa','Okada','Hasegawa','Murakami','Kondo','Endo','Sakamoto'],
 ARRAY['gmail.com','yahoo.co.jp','docomo.ne.jp','ezweb.ne.jp','softbank.ne.jp','icloud.com'],
 ARRAY['110','111','118','119','120','121','122','123','124','125','126','133']),
('kr',
 ARRAY['Minjun','Seojun','Hajun','Dohyun','Jiho','Junseo','Yechan','Hyunwoo','Jiwon','Sungmin','Donghyun','Minho','Jaehyun','Seoyeon','Jiwoo','Seoyun','Haeun','Jian','Yuna','Chaewon','Minseo','Sua','Jimin','Yejin','Hyerin','Soyeon','Nayeon'],
 ARRAY['Kim','Lee','Park','Choi','Jung','Kang','Cho','Yoon','Shin','Jang','Lim','Han','Oh','Seo','Kwon','Hwang','Ahn','Song','Hong','Jeon','Moon','Bae','Baek','Nam','Sim','Ha'],
 ARRAY['gmail.com','naver.com','daum.net','hanmail.net','kakao.com','nate.com'],
 ARRAY['110','111','112','113','114','115','116','117','118','119','120','121','175','211']),
('zh',
 ARRAY['Wei','Jun','Hao','Yu','Chen','Ming','Jian','Feng','Gang','Tao','Lei','Chao','Peng','Xin','Fang','Na','Min','Jing','Juan','Ting','Xiu','Yan','Ling','Xue','Qian','Hui','Dan'],
 ARRAY['Wang','Li','Zhang','Liu','Chen','Yang','Huang','Zhao','Wu','Zhou','Xu','Sun','Ma','Zhu','Hu','Guo','He','Lin','Luo','Zheng','Liang','Xie','Song','Tang','Han','Feng','Deng','Cao','Peng','Cheng'],
 ARRAY['qq.com','163.com','126.com','gmail.com','sina.com','outlook.com','139.com'],
 ARRAY['1','14','27','36','39','42','58','59','60','101','103','106','112','113','114','115','116','117','118','119','120','121','122','123','124','125','183']),
('in',
 ARRAY['Aarav','Vivaan','Aditya','Arjun','Reyansh','Krishna','Ishaan','Rohan','Kabir','Raj','Amit','Rahul','Vikram','Sanjay','Nikhil','Varun','Karan','Arnav','Ananya','Diya','Aadhya','Myra','Sara','Ira','Priya','Anika','Navya','Fatima','Sneha','Pooja','Neha','Kavita','Riya','Shreya','Tanvi'],
 ARRAY['Sharma','Verma','Patel','Gupta','Mehta','Singh','Kumar','Das','Shah','Reddy','Rao','Iyer','Nair','Menon','Chatterjee','Mukherjee','Banerjee','Joshi','Deshmukh','Kulkarni','Agarwal','Malhotra','Chopra','Kapoor','Bhat','Saxena','Sinha','Pandey','Mishra','Trivedi','Chauhan','Yadav','Thakur','Bhatt','Pillai'],
 ARRAY['gmail.com','yahoo.in','rediffmail.com','outlook.com','hotmail.com'],
 ARRAY['1','14','27','49','59','101','103','106','115','117','122','124']),
('arab',
 ARRAY['Mohammed','Ahmed','Ali','Omar','Khaled','Youssef','Abdullah','Abdulaziz','Fahad','Ibrahim','Hassan','Hussein','Tariq','Saeed','Karim','Zaid','Bilal','Fatima','Aisha','Mariam','Noor','Huda','Layla','Sara','Amira','Yasmin','Salma','Zainab','Rania','Dina','Mona','Heba'],
 ARRAY['AlSayed','Hassan','Hussein','Ali','Mohammed','Ahmed','Ibrahim','Khalil','Mansour','Nasser','Saleh','AlOtaibi','AlGhamdi','Khan','Rahman','AbdelAziz','ElMasry','AlRashid','Haddad','Khoury','Farah','Aziz','Osman','Mahmoud','Mostafa','ElSherif','Barakat','Darwish','Hamdan','Qassem'],
 ARRAY['gmail.com','hotmail.com','yahoo.com','outlook.com'],
 ARRAY['31','37','41','46','77','78','84','85','90','92','93','94','95','154','156','188','196','197']),
('tr',
 ARRAY['Yusuf','Eymen','Omer','Mustafa','Ahmet','Mehmet','Emir','Miran','Ali','Kerem','Berat','Aras','Zeynep','Elif','Defne','Ecrin','Asel','Azra','Eylul','Nehir','Miray','Asya','Yagmur','Melek','Esra','Merve'],
 ARRAY['Yilmaz','Kaya','Demir','Sahin','Celik','Yildiz','Yildirim','Ozturk','Aydin','Ozdemir','Arslan','Dogan','Kilic','Aslan','Cetin','Kara','Koc','Kurt','Ozkan','Simsek','Polat','Korkmaz','Erdogan','Gunes','Akman','Turan','Bulut','Ates','Yavuz','Guler'],
 ARRAY['gmail.com','hotmail.com','outlook.com','yandex.com.tr','mynet.com'],
 ARRAY['31','46','78','81','82','83','84','85','88','92','93','94','95','176','178','188']),
('sv',
 ARRAY['Liam','Noah','Hugo','Oliver','Elias','Adam','Leo','Alexander','Oscar','Axel','Alice','Maja','Elsa','Astrid','Wilma','Ella','Olivia','Alma','Freja','Agnes','Saga','Ebba','Vera','Signe'],
 ARRAY['Andersson','Johansson','Karlsson','Nilsson','Eriksson','Larsson','Olsson','Persson','Svensson','Gustafsson','Pettersson','Jonsson','Jansson','Hansson','Bengtsson','Jonsson','Lindberg','Jakobsson','Magnusson','Lindstrom','Olofsson','Lindqvist','Lindgren','Axelsson','Bergstrom','Lundberg','Lundgren','Lundqvist','Mattsson','Berglund'],
 ARRAY['gmail.com','hotmail.se','outlook.com','telia.com','bredband.net'],
 ARRAY['77','78','81','82','83','84','85','90','155','176','188','193','194','195']),
('no',
 ARRAY['Jakob','Noah','Emil','Oliver','Filip','William','Lucas','Liam','Oskar','Theodor','Nora','Emma','Ella','Maja','Olivia','Emilie','Sofie','Leah','Ingrid','Sara','Frida','Tuva','Hedda','Amalie'],
 ARRAY['Hansen','Johansen','Olsen','Larsen','Andersen','Pedersen','Nilsen','Kristiansen','Jensen','Karlsen','Johnsen','Pettersen','Eriksen','Berg','Haugen','Hagen','Johannessen','Andreassen','Jacobsen','Dahl','Jorgensen','Henriksen','Lund','Halvorsen','Sorensen','Jakobsen','Moen','Gundersen','Iversen','Strand'],
 ARRAY['gmail.com','hotmail.no','online.no','outlook.com','broadpark.no'],
 ARRAY['77','78','79','80','81','82','83','84','85','88','155','176','193']),
('da',
 ARRAY['William','Alfred','Oscar','Noah','Karl','Emil','Valdemar','Oliver','Arthur','August','Agnes','Alma','Ella','Freja','Josefine','Clara','Anna','Emma','Ida','Karla','Olivia','Sofia','Esther','Lily'],
 ARRAY['Jensen','Nielsen','Hansen','Pedersen','Andersen','Christensen','Larsen','Sorensen','Rasmussen','Jorgensen','Petersen','Madsen','Kristensen','Olsen','Thomsen','Christiansen','Poulsen','Johansen','Moller','Mortensen','Frederiksen','Vestergaard','Laursen','Berg','Simonsen','Klausen','Holm','Bruun','Skov','Lund'],
 ARRAY['gmail.com','hotmail.dk','outlook.dk','stofa.dk','mail.dk'],
 ARRAY['77','78','79','80','83','85','87','93','176','188','193']),
('fi',
 ARRAY['Oliver','Elias','Onni','Leo','Vaino','Eino','Noel','Leevi','Aleksi','Veikko','Aino','Sofia','Eevi','Venla','Pihla','Aada','Ella','Helmi','Ellen','Isla','Alma','Oona','Hilla','Sanni'],
 ARRAY['Korhonen','Virtanen','Makinen','Nieminen','Makela','Hamalainen','Laine','Heikkinen','Koskinen','Jarvinen','Lehtonen','Lehtinen','Saarinen','Salminen','Heinonen','Niemi','Heikkila','Kinnunen','Salonen','Turunen','Salo','Laitinen','Rantanen','Tuominen','Karjalainen','Mattila','Jokinen','Savolainen','Laaksonen','Ahonen'],
 ARRAY['gmail.com','hotmail.fi','outlook.com','elisanet.fi','kolumbus.fi'],
 ARRAY['84','85','87','88','91','93','109','176','188','193']),
('cs',
 ARRAY['Jakub','Jan','Tomas','Adam','Matyas','Vojtech','Filip','Ondrej','Dominik','Martin','Eliska','Anna','Adela','Tereza','Karolina','Natalie','Emma','Sofie','Kristyna','Barbora','Veronika','Lucie','Katerina','Marketa'],
 ARRAY['Novak','Svoboda','Novotny','Dvorak','Cerny','Prochazka','Kucera','Vesely','Horak','Nemec','Marek','Pospisil','Pokorny','Hajek','Kral','Jelinek','Ruzicka','Benes','Fiala','Sedlacek','Dolezal','Zeman','Kolar','Navratil','Cermak','Urban','Vanek','Blaha','Kopecky'],
 ARRAY['gmail.com','seznam.cz','centrum.cz','email.cz','post.cz','atlas.cz'],
 ARRAY['77','78','83','85','88','89','90','91','93','109','176','188','193','194','195']),
('hu',
 ARRAY['Bence','Mate','Levente','Dominik','Marcell','Daniel','Adam','Milan','Zsombor','Kristof','Hanna','Anna','Zoe','Lena','Emma','Luca','Boglarka','Jazmin','Csenge','Dora','Fanni','Lili','Nora','Viktoria'],
 ARRAY['Nagy','Kovacs','Toth','Szabo','Horvath','Varga','Kiss','Molnar','Nemeth','Farkas','Balogh','Papp','Takacs','Juhasz','Lakatos','Meszaros','Olah','Simon','Racz','Fekete','Szilagyi','Torok','Feher','Balazs','Gal','Szucs','Kocsis','Orsos','Pinter','Szekely'],
 ARRAY['gmail.com','freemail.hu','citromail.hu','outlook.com','hotmail.com'],
 ARRAY['77','78','79','84','85','86','87','92','94','188','193']),
('ro',
 ARRAY['Andrei','Alexandru','Stefan','Gabriel','Mihai','David','Ionut','Cristian','Darius','Robert','Maria','Elena','Ioana','Andreea','Alexandra','Ana','Sofia','Gabriela','Daria','Antonia','Bianca','Denisa','Raluca','Teodora'],
 ARRAY['Popa','Popescu','Radu','Dumitru','Stan','Stoica','Gheorghe','Matei','Ciobanu','Ionescu','Rusu','Moldovan','Marin','Tudor','Dobre','Barbu','Nistor','Florea','Manea','Dima','Cristea','Georgescu','Oprea','Sandu','Enache','Preda','Ilie','Mocanu','Stanciu','Dinu'],
 ARRAY['gmail.com','yahoo.com','outlook.com','hotmail.com','rdslink.ro'],
 ARRAY['79','82','83','84','85','86','89','92','93','109','188']),
('el',
 ARRAY['Giorgos','Dimitris','Konstantinos','Nikolaos','Ioannis','Panagiotis','Vasilios','Christos','Maria','Eleni','Aikaterini','Vasiliki','Sofia','Georgia','Dimitra','Ioanna','Paraskevi','Christina'],
 ARRAY['Papadopoulos','Papadakis','Georgiou','Konstantinou','Dimitriou','Nikolaou','Ioannidis','Pappas','Vasileiou','Makris','Antoniou','Christou','Economou','Karagiannis','Stavropoulos','Theodorou','Nikolaidis','Athanasiou','Samaras','Kokkinos'],
 ARRAY['gmail.com','yahoo.gr','hotmail.gr','outlook.com','otenet.gr','forthnet.gr'],
 ARRAY['77','79','84','85','87','92','94','109','176','188']),
('th',
 ARRAY['Somchai','Somsak','Nattapong','Thanakit','Arthit','Kittipong','Chaiwat','Pongpat','Malee','Nongnuch','Siriporn','Kanokwan','Pimchanok','Supaporn','Natthaya','Waranya','Chonticha'],
 ARRAY['Saetang','Srisai','Boonmee','Chaiyasan','Phasuk','Intarapong','Wongsuwan','Sombat','Thongdee','Kaewmanee','Rattanapong','Sukhothai','Chaiyaporn','Phetcharat','Suwannee'],
 ARRAY['gmail.com','hotmail.com','outlook.com','yahoo.com'],
 ARRAY['1','27','49','58','61','101','103','110','112','113','114','115','116','117','118','119','120','121','122','123','124','125','171','182']),
('vi',
 ARRAY['Anh','Minh','Tuan','Hung','Duc','Long','Huy','Khoa','Nam','Phuc','Dat','Quang','Lan','Mai','Huong','Thao','Linh','Ngoc','Trang','Thu','Hien','Phuong','Vy','Nhi'],
 ARRAY['Nguyen','Tran','Le','Pham','Hoang','Phan','Vu','Vo','Dang','Bui','Do','Ho','Ngo','Duong','Ly','Trinh','Dinh','Lam','Mai','Cao'],
 ARRAY['gmail.com','yahoo.com','outlook.com','hotmail.com'],
 ARRAY['1','14','27','42','58','113','114','115','116','117','118','119','120','123','125','171','183']),
('id',
 ARRAY['Budi','Agus','Rizky','Putra','Dian','Dewi','Siti','Ayu','Rina','Fitri','Muhammad','Ahmad','Eko','Bayu','Aditya','Fajar','Sari','Wulandari','Lestari','Ningsih','Andi','Rudi','Dedi','Irfan'],
 ARRAY['Santoso','Wijaya','Pratama','Saputra','Kurniawan','Hidayat','Nugroho','Susanto','Hartono','Setiawan','Wibowo','Firmansyah','Ramadhan','Gunawan','Permana','Siregar','Simanjuntak','Hutapea','Nasution','Pangestu'],
 ARRAY['gmail.com','yahoo.co.id','outlook.com','hotmail.com'],
 ARRAY['36','66','103','110','111','112','113','114','115','116','117','118','119','120','121','122','123','124','125','139','140','149','180','182']),
('west_africa',
 ARRAY['Kwame','Kofi','Chidi','Emeka','Olufemi','Adebayo','Tunde','Ibrahim','Kwabena','Kelechi','Obinna','Segun','Ngozi','Chioma','Adaeze','Fatou','Amina','Abena','Efua','Yaa','Akosua','Funke','Bola','Nneka'],
 ARRAY['Okafor','Adeyemi','Mensah','Owusu','Boateng','Asante','Osei','Diallo','Traore','Ogunleye','Balogun','Eze','Nwachukwu','Adeleke','Ojo','Bello','Musa','Abubakar','Danjuma','Appiah','Ibrahim','Okonkwo','Adebayo','Olawale','Osei','Nkemdirim','Chukwu','Afolabi','Onyeka','Owolabi'],
 ARRAY['gmail.com','yahoo.com','outlook.com','hotmail.com'],
 ARRAY['41','102','105','154','155','156','196','197','102','129']),
('east_africa',
 ARRAY['Mwangi','Kamau','Otieno','Kipchoge','Tesfaye','Abebe','Haile','Juma','Baraka','Wanjiku','Njeri','Achieng','Nyambura','Zawadi','Neema','Rehema','Wairimu','Abeba','Hiwot','Selam'],
 ARRAY['Kariuki','Njoroge','Odhiambo','Omondi','Waweru','Muthoni','Kiplagat','Tadesse','Bekele','Girma','Mekonnen','Alemu','Kimani','Wafula','Simiyu','Chebet','Kibet','Wanjiru','Mwangi','Gitonga','Mbugua','Nyaguthii','Osei','Temesgen','Worku'],
 ARRAY['gmail.com','yahoo.com','outlook.com','hotmail.com'],
 ARRAY['41','105','154','196','197','102','129','212']),
('he',
 ARRAY['Noam','Itai','Uri','Ariel','Yosef','David','Eitan','Omer','Daniel','Yoav','Noa','Tamar','Shira','Maya','Yael','Avigail','Talia','Sarah','Roni','Lia'],
 ARRAY['Cohen','Levi','Mizrahi','Peretz','Biton','Dahan','Avraham','Friedman','Azulay','Katz','Shapira','BenDavid','Ohayon','Gabay','Amar','Levy','Malka','Segal','Goldstein','Baruch'],
 ARRAY['gmail.com','walla.co.il','outlook.com','hotmail.com','013net.net'],
 ARRAY['84','85','87','93','94','109','176','188','212']),
('ms',
 ARRAY['Ahmad','Muhammad','Nurul','Siti','Aisyah','Farah','Amir','Hakim','Daniel','Aiman','Zara','Iman','Hafiz','Syafiq','Afiq','Izzat','Balqis','Ain'],
 ARRAY['Abdullah','Rahman','Ismail','Yusof','Hassan','Osman','Ibrahim','Mohamed','Ali','Ahmad','Razak','Hamid','Karim','Aziz','Majid','Salleh','Othman','Bakar'],
 ARRAY['gmail.com','yahoo.com','outlook.com','hotmail.com','tm.net.my'],
 ARRAY['1','14','27','42','60','103','110','113','114','115','116','118','119','120','121','123','124','175']),
('fil',
 ARRAY['Jose','Juan','Mark','JohnRey','Angelo','Carlo','Paolo','Miguel','Rafael','Maria','Angel','Nicole','Princess','Christine','Joy','Rica','Kristine','Jessa','MaryAnn','Jerome'],
 ARRAY['Santos','Reyes','Cruz','Bautista','Ocampo','Garcia','Mendoza','Torres','Tomas','Andrada','Castillo','Flores','Villanueva','Ramos','Aquino','Navarro','Mercado','Aguilar','DelRosario','Gonzales','Fernandez','Lopez','Pascual','Santiago','Domingo','Marquez'],
 ARRAY['gmail.com','yahoo.com','outlook.com','hotmail.com','pldthome.net'],
 ARRAY['49','112','113','114','115','116','117','118','119','120','121','122','124','125','136','152','180']),
('balkan',
 ARRAY['Luka','Marko','Ivan','Jovan','Stefan','Nikola','Petar','Filip','Milica','Ana','Jelena','Ivana','Petra','Marija','Katarina','Teodora','Dunja','Sara'],
 ARRAY['Jovanovic','Petrovic','Kovacevic','Nikolic','Horvat','Kovac','Babic','Maric','Juric','Novak','Stojanovic','Ilic','Pavlovic','Markovic','Djordjevic','Popovic','Vukovic','Knezevic','Grbic','Matic'],
 ARRAY['gmail.com','outlook.com','hotmail.com','yahoo.com','eunet.rs'],
 ARRAY['77','79','82','84','85','87','89','91','93','94','109','178','188']),
('baltic',
 ARRAY['Janis','Arturs','Martins','Edgars','Kristaps','Jonas','Marius','Lukas','Anna','Kristine','Laura','Egle','Gabija','Emilija','Ieva','Rugile'],
 ARRAY['Berzins','Kalnins','Ozolins','Jansons','Petraitis','Kazlauskas','Stankevicius','Vaitkus','Zukauskas','Butkus','Liepa','Rudzitis','Vanags','Sakalauskas','Urbonas','Navickas'],
 ARRAY['gmail.com','inbox.lv','mail.lt','outlook.com','hotmail.com'],
 ARRAY['77','78','83','84','85','87','88','92','188','193']),
('za',
 ARRAY['Sipho','Thabo','Johan','Pieter','Liam','Jaco','Naledi','Lerato','Zanele','Anika','Emma','Karabo','Thandiwe','Ruan','Mieke','Christiaan'],
 ARRAY['vanderMerwe','Botha','Nkosi','Dlamini','Mokoena','Pretorius','Sithole','Ndlovu','Venter','Coetzee','Fourie','Steyn','Mahlangu','Zulu','Khuzwayo','Erasmus'],
 ARRAY['gmail.com','outlook.com','yahoo.com','mweb.co.za','afrihost.co.za'],
 ARRAY['41','102','105','154','155','156','196','197']),
('fa',
 ARRAY['Ali','Reza','Mohammad','Hossein','Amir','Mehdi','Saeed','Omid','Fatemeh','Zahra','Maryam','Sara','Narges','Shirin','Niloufar','Yasaman'],
 ARRAY['Hosseini','Ahmadi','Mohammadi','Rezaei','Karimi','Moradi','Jafari','Ghasemi','Ebrahimi','Rahimi','Akbari','Salehi','Nikbakht','Farhadi','Azizi','Taheri'],
 ARRAY['gmail.com','yahoo.com','outlook.com','hotmail.com'],
 ARRAY['2','5','31','37','46','78','80','82','85','86','91','93','95','151','188']),
('is',
 ARRAY['Jon','Bjorn','Sigurdur','Gunnar','Einar','Ari','Bjork','Sigridur','Anna','Kristin','Guorun','Helga','Eva','Katrin'],
 ARRAY['Jonsson','Sigurdsson','Bjornsson','Gunnarsson','Einarsson','Olafsson','Magnusson','Jonsdottir','Sigurdsdottir','Bjornsdottir','Gunnarsdottir','Olafsdottir','Magnusdottir','Johansson'],
 ARRAY['gmail.com','hotmail.com','outlook.com','simnet.is'],
 ARRAY['82','85','88','92','109','176','185','193']);

-- ============================================================================
-- Reference data: games, categories, countries
-- ============================================================================
\echo '=== Loading games / categories / countries ==='

INSERT INTO games (id, name, short_name, developer, publisher, release_date, genre, mod_weight) VALUES
 (1,'Grand Theft Auto V','GTA V','Rockstar North','Rockstar Games','2013-09-17','Action-adventure',25),
 (2,'Tom Clancy''s Rainbow Six Siege','R6 Siege','Ubisoft Montreal','Ubisoft','2015-12-01','Tactical FPS',15),
 (3,'Minecraft','Minecraft','Mojang Studios','Mojang Studios','2011-11-18','Sandbox',12),
 (4,'The Elder Scrolls V: Skyrim','Skyrim','Bethesda Game Studios','Bethesda Softworks','2011-11-11','RPG',8),
 (5,'Counter-Strike 2','CS2','Valve','Valve','2023-09-27','FPS',5),
 (6,'Cyberpunk 2077','Cyberpunk 2077','CD Projekt Red','CD Projekt','2020-12-10','RPG',5),
 (7,'Elden Ring','Elden Ring','FromSoftware','Bandai Namco','2022-02-25','Action RPG',4),
 (8,'Red Dead Redemption 2','RDR2','Rockstar Studios','Rockstar Games','2018-10-26','Action-adventure',3),
 (9,'The Witcher 3: Wild Hunt','Witcher 3','CD Projekt Red','CD Projekt','2015-05-19','RPG',3),
 (10,'Fallout 4','Fallout 4','Bethesda Game Studios','Bethesda Softworks','2015-11-10','RPG',3),
 (11,'Baldur''s Gate 3','BG3','Larian Studios','Larian Studios','2023-08-03','CRPG',3),
 (12,'ARMA 3','ARMA 3','Bohemia Interactive','Bohemia Interactive','2013-09-12','Mil-sim',2),
 (13,'DayZ','DayZ','Bohemia Interactive','Bohemia Interactive','2018-12-13','Survival',2),
 (14,'Rust','Rust','Facepunch Studios','Facepunch Studios','2018-02-08','Survival',2.5),
 (15,'Valheim','Valheim','Iron Gate Studio','Coffee Stain Publishing','2021-02-02','Survival',2),
 (16,'Stardew Valley','Stardew Valley','ConcernedApe','ConcernedApe','2016-02-26','Farming sim',1.5),
 (17,'Factorio','Factorio','Wube Software','Wube Software','2020-08-14','Automation',1.5),
 (18,'Cities: Skylines','Cities Skylines','Colossal Order','Paradox Interactive','2015-03-10','City-builder',1.5),
 (19,'Euro Truck Simulator 2','ETS2','SCS Software','SCS Software','2012-10-19','Simulation',1.5),
 (20,'The Sims 4','Sims 4','Maxis','Electronic Arts','2014-09-02','Life sim',2);

INSERT INTO categories (id, name) VALUES
 (1,'Vehicles'),(2,'Weapons'),(3,'Maps'),(4,'Skins & Textures'),(5,'Graphics & ENB'),
 (6,'Gameplay'),(7,'Scripts'),(8,'Audio'),(9,'UI & HUD'),(10,'Characters');

--  code, name, currency, fx/USD, vat, locale_group, player_weight, buyer_propensity, cities
INSERT INTO countries (code, name, currency, fx_rate, vat_rate, locale_group, player_weight, buyer_propensity, cities) VALUES
 ('US','United States','USD',1.0,0.08,'en_us',13.5,1.30,ARRAY['New York','Los Angeles','Chicago','Austin','Seattle','Denver','Miami','Atlanta']),
 ('CA','Canada','CAD',1.37,0.12,'en_us',3.5,1.25,ARRAY['Toronto','Vancouver','Montreal','Calgary']),
 ('MX','Mexico','MXN',18.5,0.16,'es_latam',3.0,0.85,ARRAY['Mexico City','Guadalajara','Monterrey']),
 ('GT','Guatemala','GTQ',7.75,0.12,'es_latam',0.15,0.60,ARRAY['Guatemala City']),
 ('CR','Costa Rica','CRC',520.0,0.13,'es_latam',0.15,0.75,ARRAY['San Jose']),
 ('PA','Panama','USD',1.0,0.07,'es_latam',0.10,0.70,ARRAY['Panama City']),
 ('DO','Dominican Republic','DOP',59.0,0.18,'es_latam',0.15,0.55,ARRAY['Santo Domingo']),
 ('JM','Jamaica','JMD',156.0,0.15,'en_gb',0.05,0.55,ARRAY['Kingston']),
 ('TT','Trinidad and Tobago','TTD',6.8,0.125,'en_gb',0.05,0.60,ARRAY['Port of Spain']),
 ('CU','Cuba','CUP',24.0,0.10,'es_latam',0.05,0.35,ARRAY['Havana']),
 ('BR','Brazil','BRL',5.45,0.17,'pt_br',6.0,0.90,ARRAY['Sao Paulo','Rio de Janeiro','Curitiba','Belo Horizonte','Porto Alegre','Salvador']),
 ('AR','Argentina','ARS',920.0,0.21,'es_latam',2.0,0.75,ARRAY['Buenos Aires','Cordoba','Rosario']),
 ('CO','Colombia','COP',4100.0,0.19,'es_latam',1.2,0.70,ARRAY['Bogota','Medellin','Cali']),
 ('CL','Chile','CLP',930.0,0.19,'es_latam',0.9,0.80,ARRAY['Santiago','Valparaiso']),
 ('PE','Peru','PEN',3.7,0.18,'es_latam',0.7,0.65,ARRAY['Lima','Arequipa']),
 ('VE','Venezuela','USD',1.0,0.16,'es_latam',0.3,0.45,ARRAY['Caracas','Maracaibo']),
 ('EC','Ecuador','USD',1.0,0.12,'es_latam',0.3,0.55,ARRAY['Quito','Guayaquil']),
 ('UY','Uruguay','UYU',40.0,0.22,'es_latam',0.15,0.85,ARRAY['Montevideo']),
 ('PY','Paraguay','PYG',7500.0,0.10,'es_latam',0.10,0.50,ARRAY['Asuncion']),
 ('BO','Bolivia','BOB',6.9,0.13,'es_latam',0.10,0.45,ARRAY['La Paz','Santa Cruz']),
 ('GY','Guyana','GYD',209.0,0.14,'en_gb',0.03,0.45,ARRAY['Georgetown']),
 ('SR','Suriname','SRD',30.0,0.10,'nl',0.02,0.45,ARRAY['Paramaribo']),
 ('GB','United Kingdom','GBP',0.79,0.20,'en_gb',5.0,1.25,ARRAY['London','Manchester','Birmingham','Leeds','Glasgow']),
 ('DE','Germany','EUR',0.92,0.19,'de',5.0,1.30,ARRAY['Berlin','Frankfurt','Munich','Hamburg','Cologne']),
 ('FR','France','EUR',0.92,0.20,'fr',4.0,1.20,ARRAY['Paris','Lyon','Marseille','Toulouse']),
 ('NL','Netherlands','EUR',0.92,0.21,'nl',1.8,1.30,ARRAY['Amsterdam','Rotterdam','Utrecht','Eindhoven']),
 ('BE','Belgium','EUR',0.92,0.21,'fr',0.6,1.20,ARRAY['Brussels','Antwerp','Ghent']),
 ('LU','Luxembourg','EUR',0.92,0.17,'fr',0.05,1.30,ARRAY['Luxembourg City']),
 ('CH','Switzerland','CHF',0.88,0.077,'de',0.6,1.30,ARRAY['Zurich','Geneva','Bern']),
 ('AT','Austria','EUR',0.92,0.20,'de',0.6,1.20,ARRAY['Vienna','Graz','Linz']),
 ('ES','Spain','EUR',0.92,0.21,'es_es',2.5,1.00,ARRAY['Madrid','Barcelona','Valencia','Seville']),
 ('PT','Portugal','EUR',0.92,0.23,'pt_pt',0.8,0.95,ARRAY['Lisbon','Porto']),
 ('IT','Italy','EUR',0.92,0.22,'it',2.0,1.00,ARRAY['Rome','Milan','Naples','Turin']),
 ('MT','Malta','EUR',0.92,0.18,'en_gb',0.03,1.00,ARRAY['Valletta']),
 ('SE','Sweden','SEK',10.5,0.25,'sv',1.8,1.30,ARRAY['Stockholm','Gothenburg','Malmo']),
 ('NO','Norway','NOK',10.8,0.25,'no',0.8,1.35,ARRAY['Oslo','Bergen','Trondheim']),
 ('DK','Denmark','DKK',6.9,0.25,'da',0.8,1.30,ARRAY['Copenhagen','Aarhus','Odense']),
 ('FI','Finland','EUR',0.92,0.24,'fi',0.7,1.25,ARRAY['Helsinki','Tampere','Turku']),
 ('IS','Iceland','ISK',138.0,0.24,'is',0.08,1.25,ARRAY['Reykjavik']),
 ('IE','Ireland','EUR',0.92,0.23,'en_gb',0.6,1.20,ARRAY['Dublin','Cork']),
 ('PL','Poland','PLN',3.9,0.23,'pl',3.0,1.00,ARRAY['Warsaw','Krakow','Gdansk','Wroclaw']),
 ('CZ','Czechia','CZK',23.0,0.21,'cs',0.7,1.00,ARRAY['Prague','Brno','Ostrava']),
 ('SK','Slovakia','EUR',0.92,0.20,'cs',0.25,0.90,ARRAY['Bratislava','Kosice']),
 ('HU','Hungary','HUF',360.0,0.27,'hu',0.5,0.90,ARRAY['Budapest','Debrecen']),
 ('RO','Romania','RON',4.6,0.19,'ro',0.7,0.85,ARRAY['Bucharest','Cluj-Napoca','Timisoara']),
 ('BG','Bulgaria','BGN',1.8,0.20,'ru',0.2,0.75,ARRAY['Sofia','Plovdiv']),
 ('GR','Greece','EUR',0.92,0.24,'el',0.5,0.85,ARRAY['Athens','Thessaloniki']),
 ('HR','Croatia','EUR',0.92,0.25,'balkan',0.15,0.90,ARRAY['Zagreb','Split']),
 ('RS','Serbia','RSD',108.0,0.20,'balkan',0.15,0.70,ARRAY['Belgrade','Novi Sad']),
 ('SI','Slovenia','EUR',0.92,0.22,'balkan',0.05,1.00,ARRAY['Ljubljana']),
 ('BA','Bosnia and Herzegovina','BAM',1.8,0.17,'balkan',0.08,0.65,ARRAY['Sarajevo']),
 ('MK','North Macedonia','MKD',56.0,0.18,'balkan',0.05,0.60,ARRAY['Skopje']),
 ('AL','Albania','ALL',95.0,0.20,'balkan',0.05,0.60,ARRAY['Tirana']),
 ('UA','Ukraine','UAH',41.0,0.20,'uk',1.2,0.70,ARRAY['Kyiv','Lviv','Kharkiv','Odesa']),
 ('RU','Russia','RUB',90.0,0.20,'ru',3.5,0.85,ARRAY['Moscow','Saint Petersburg','Novosibirsk','Yekaterinburg','Kazan']),
 ('BY','Belarus','BYN',3.2,0.20,'ru',0.2,0.65,ARRAY['Minsk']),
 ('EE','Estonia','EUR',0.92,0.20,'fi',0.08,1.00,ARRAY['Tallinn','Tartu']),
 ('LV','Latvia','EUR',0.92,0.21,'baltic',0.08,0.90,ARRAY['Riga']),
 ('LT','Lithuania','EUR',0.92,0.21,'baltic',0.10,0.90,ARRAY['Vilnius','Kaunas']),
 ('MD','Moldova','MDL',17.7,0.20,'ro',0.05,0.60,ARRAY['Chisinau']),
 ('CY','Cyprus','EUR',0.92,0.19,'el',0.05,0.95,ARRAY['Nicosia','Limassol']),
 ('TR','Turkey','TRY',33.0,0.18,'tr',1.8,0.75,ARRAY['Istanbul','Ankara','Izmir','Bursa']),
 ('GE','Georgia','GEL',2.7,0.18,'ru',0.08,0.65,ARRAY['Tbilisi']),
 ('AM','Armenia','AMD',390.0,0.20,'ru',0.05,0.60,ARRAY['Yerevan']),
 ('AZ','Azerbaijan','AZN',1.7,0.18,'tr',0.08,0.65,ARRAY['Baku']),
 ('JP','Japan','JPY',155.0,0.10,'jp',4.0,1.15,ARRAY['Tokyo','Osaka','Nagoya','Fukuoka','Sapporo']),
 ('KR','South Korea','KRW',1370.0,0.10,'kr',2.5,1.15,ARRAY['Seoul','Busan','Incheon']),
 ('CN','China','CNY',7.2,0.13,'zh',2.0,0.90,ARRAY['Shanghai','Beijing','Shenzhen','Guangzhou','Chengdu']),
 ('TW','Taiwan','TWD',32.0,0.05,'zh',0.8,1.05,ARRAY['Taipei','Kaohsiung']),
 ('HK','Hong Kong','HKD',7.8,0.0,'zh',0.5,1.10,ARRAY['Hong Kong']),
 ('MO','Macau','MOP',8.0,0.0,'zh',0.05,1.00,ARRAY['Macau']),
 ('IN','India','INR',83.5,0.18,'in',3.0,0.65,ARRAY['Mumbai','Delhi','Bangalore','Hyderabad','Chennai','Pune']),
 ('PK','Pakistan','PKR',278.0,0.17,'arab',0.3,0.50,ARRAY['Karachi','Lahore','Islamabad']),
 ('BD','Bangladesh','BDT',117.0,0.15,'in',0.15,0.45,ARRAY['Dhaka','Chittagong']),
 ('LK','Sri Lanka','LKR',300.0,0.15,'in',0.08,0.50,ARRAY['Colombo']),
 ('NP','Nepal','NPR',133.0,0.13,'in',0.05,0.45,ARRAY['Kathmandu']),
 ('TH','Thailand','THB',36.0,0.07,'th',0.8,0.70,ARRAY['Bangkok','Chiang Mai','Phuket']),
 ('VN','Vietnam','VND',25400.0,0.10,'vi',0.7,0.65,ARRAY['Ho Chi Minh City','Hanoi','Da Nang']),
 ('MY','Malaysia','MYR',4.7,0.06,'ms',0.8,0.75,ARRAY['Kuala Lumpur','Penang','Johor Bahru']),
 ('SG','Singapore','SGD',1.34,0.09,'zh',0.7,1.15,ARRAY['Singapore']),
 ('ID','Indonesia','IDR',16200.0,0.11,'id',1.5,0.60,ARRAY['Jakarta','Surabaya','Bandung','Medan']),
 ('PH','Philippines','PHP',58.0,0.12,'fil',1.5,0.60,ARRAY['Manila','Quezon City','Cebu','Davao']),
 ('BN','Brunei','BND',1.34,0.0,'ms',0.03,0.80,ARRAY['Bandar Seri Begawan']),
 ('KZ','Kazakhstan','KZT',470.0,0.12,'ru',0.2,0.70,ARRAY['Almaty','Astana']),
 ('UZ','Uzbekistan','UZS',12600.0,0.15,'ru',0.10,0.55,ARRAY['Tashkent']),
 ('MN','Mongolia','MNT',3400.0,0.10,'ru',0.05,0.55,ARRAY['Ulaanbaatar']),
 ('MM','Myanmar','MMK',2100.0,0.05,'th',0.05,0.40,ARRAY['Yangon']),
 ('KH','Cambodia','KHR',4100.0,0.10,'th',0.05,0.45,ARRAY['Phnom Penh']),
 ('LA','Laos','LAK',21500.0,0.10,'th',0.03,0.40,ARRAY['Vientiane']),
 ('SA','Saudi Arabia','SAR',3.75,0.15,'arab',1.0,1.00,ARRAY['Riyadh','Jeddah','Dammam']),
 ('AE','United Arab Emirates','AED',3.67,0.05,'arab',0.7,1.10,ARRAY['Dubai','Abu Dhabi','Sharjah']),
 ('QA','Qatar','QAR',3.64,0.0,'arab',0.15,1.05,ARRAY['Doha']),
 ('KW','Kuwait','KWD',0.31,0.0,'arab',0.15,1.00,ARRAY['Kuwait City']),
 ('BH','Bahrain','BHD',0.38,0.10,'arab',0.05,0.90,ARRAY['Manama']),
 ('OM','Oman','OMR',0.39,0.05,'arab',0.08,0.85,ARRAY['Muscat']),
 ('IL','Israel','ILS',3.7,0.17,'he',0.4,1.05,ARRAY['Tel Aviv','Jerusalem','Haifa']),
 ('JO','Jordan','JOD',0.71,0.16,'arab',0.08,0.65,ARRAY['Amman']),
 ('LB','Lebanon','LBP',89500.0,0.11,'arab',0.08,0.55,ARRAY['Beirut']),
 ('IQ','Iraq','IQD',1310.0,0.0,'arab',0.10,0.45,ARRAY['Baghdad','Erbil']),
 ('IR','Iran','IRR',42000.0,0.09,'fa',0.20,0.50,ARRAY['Tehran','Mashhad','Isfahan']),
 ('ZA','South Africa','ZAR',18.0,0.15,'za',0.9,0.75,ARRAY['Johannesburg','Cape Town','Durban']),
 ('NG','Nigeria','NGN',1500.0,0.075,'west_africa',0.5,0.50,ARRAY['Lagos','Abuja','Ibadan']),
 ('EG','Egypt','EGP',48.0,0.14,'arab',0.6,0.55,ARRAY['Cairo','Alexandria','Giza']),
 ('KE','Kenya','KES',129.0,0.16,'east_africa',0.15,0.55,ARRAY['Nairobi','Mombasa']),
 ('GH','Ghana','GHS',15.0,0.15,'west_africa',0.12,0.55,ARRAY['Accra','Kumasi']),
 ('MA','Morocco','MAD',10.0,0.20,'arab',0.20,0.60,ARRAY['Casablanca','Rabat','Marrakesh']),
 ('TN','Tunisia','TND',3.1,0.19,'arab',0.08,0.55,ARRAY['Tunis']),
 ('DZ','Algeria','DZD',134.0,0.19,'arab',0.15,0.50,ARRAY['Algiers','Oran']),
 ('LY','Libya','LYD',4.8,0.0,'arab',0.03,0.40,ARRAY['Tripoli']),
 ('ET','Ethiopia','ETB',57.0,0.15,'east_africa',0.08,0.40,ARRAY['Addis Ababa']),
 ('UG','Uganda','UGX',3700.0,0.18,'east_africa',0.05,0.40,ARRAY['Kampala']),
 ('SN','Senegal','XOF',600.0,0.18,'west_africa',0.05,0.45,ARRAY['Dakar']),
 ('CI','Ivory Coast','XOF',600.0,0.20,'west_africa',0.05,0.45,ARRAY['Abidjan']),
 ('CM','Cameroon','XAF',600.0,0.1925,'west_africa',0.05,0.40,ARRAY['Douala','Yaounde']),
 ('MU','Mauritius','MUR',46.0,0.15,'in',0.03,0.75,ARRAY['Port Louis']),
 ('RW','Rwanda','RWF',1300.0,0.18,'east_africa',0.03,0.40,ARRAY['Kigali']),
 ('BW','Botswana','BWP',13.5,0.14,'east_africa',0.03,0.55,ARRAY['Gaborone']),
 ('NA','Namibia','NAD',18.0,0.15,'en_gb',0.03,0.55,ARRAY['Windhoek']),
 ('ZM','Zambia','ZMW',25.0,0.16,'east_africa',0.03,0.40,ARRAY['Lusaka']),
 ('ZW','Zimbabwe','USD',1.0,0.15,'east_africa',0.03,0.35,ARRAY['Harare']),
 ('TZ','Tanzania','TZS',2500.0,0.18,'east_africa',0.05,0.40,ARRAY['Dar es Salaam']),
 ('MZ','Mozambique','MZN',64.0,0.16,'pt_pt',0.03,0.35,ARRAY['Maputo']),
 ('AO','Angola','AOA',830.0,0.14,'pt_pt',0.05,0.40,ARRAY['Luanda']),
 ('AU','Australia','AUD',1.5,0.10,'en_gb',3.0,1.30,ARRAY['Sydney','Melbourne','Brisbane','Perth','Adelaide']),
 ('NZ','New Zealand','NZD',1.65,0.15,'en_gb',0.8,1.25,ARRAY['Auckland','Wellington','Christchurch']),
 ('FJ','Fiji','FJD',2.25,0.15,'en_gb',0.03,0.55,ARRAY['Suva']),
 ('PG','Papua New Guinea','PGK',3.9,0.10,'en_gb',0.02,0.35,ARRAY['Port Moresby']);

-- ============================================================================
-- Creators (600 mod authors)
-- ============================================================================
\echo '=== Seeding creators ==='

CREATE TEMP TABLE country_slots AS
SELECT array_agg(code ORDER BY random()) AS slots
FROM (
  SELECT c.code
  FROM countries c
  CROSS JOIN LATERAL generate_series(1, greatest(1, round(c.player_weight * 100)::int)) g
) s;

WITH pick AS (
  SELECT g,
         cs.slots[1 + floor(random() * cardinality(cs.slots))::int] AS ccode
  FROM generate_series(1, 600) g
  CROSS JOIN country_slots cs
)
INSERT INTO creators (id, handle, display_name, email, country_code, payout_method, is_verified, joined_at)
SELECT
  p.g,
  lower((ARRAY['Dark','Shadow','Silent','Neon','Crimson','Ghost','Turbo','Pixel','Cyber','Rogue','Iron','Mystic','Blazing','Frozen','Savage','Epic','Lone','Hyper','Void','Apex'])[1+floor(random()*20)::int]
     || (ARRAY['Forge','Works','Lab','Labs','Studio','Studios','Mods','Modding','Craft','Designs','Tech','Byte','Smith','Werks','Factory'])[1+floor(random()*15)::int]
     || (100 + (p.g * 7919) % 89900)),
  initcap(lower((ARRAY['Dark','Shadow','Silent','Neon','Crimson','Ghost','Turbo','Pixel','Cyber','Rogue','Iron','Mystic','Blazing','Frozen','Savage','Epic','Lone','Hyper','Void','Apex'])[1+floor(random()*20)::int])
     || ' ' || (ARRAY['Forge','Works','Lab','Labs','Studio','Studios','Mods','Modding','Craft','Designs','Tech','Byte','Smith','Werks','Factory'])[1+floor(random()*15)::int]),
  'studio' || p.g || '@' || (ARRAY['gmail.com','outlook.com','proton.me','yahoo.com'])[1+floor(random()*4)::int],
  p.ccode,
  (ARRAY['paypal','paypal','paypal','bank','bank','crypto'])[1+floor(random()*6)::int],
  random() < 0.72,
  '2019-01-01'::timestamptz + random() * (timestamptz '2022-06-01' - timestamptz '2019-01-01')
FROM pick p;

-- ============================================================================
-- Users (50,000)
-- ============================================================================
\echo '=== Seeding 50,000 users ==='

CREATE TEMP TABLE uname_parts AS
SELECT
 ARRAY['Dark','Shadow','Silent','Toxic','Neon','Crimson','Ghost','Alpha','Omega','Turbo','Pixel','Cyber','Rogue','Iron','Mystic','Blazing','Frozen','Savage','Epic','Lone','Night','Steel','Venom','Chaos','Zero'] AS adj,
 ARRAY['Wolf','Reaper','Viper','Falcon','Dragon','Slayer','Hunter','Knight','Ninja','Sniper','Titan','Phoenix','Raven','Storm','Blade','Cobra','Lynx','Panther','Wraith','Fox','Hawk','Bear','Tiger','Shark','Eagle'] AS noun;

INSERT INTO users (id, username, email, password_hash, first_name, last_name, display_name,
                   country_code, region, signup_ip, is_verified, created_at, last_login_at)
SELECT
  id,
  lower(uname_base) || suffix AS username,
  translate(lower(replace(replace(fn || '.' || ln, ' ', ''), '''', '')),
            'áàâãäåéèêëíìîïóòôõöøúùûüçñśćżźłńąęščžřďťňěůőű',
            'aaaaaaeeeeiiiioooooouuuucnsczzlnaesczrdtneuou')
    || suffix || '@' || domain AS email,
  md5(id::text || ':modstore') AS password_hash,
  fn, ln, fn || ' ' || ln AS display_name,
  ccode,
  region,
  signup_ip,
  random() < 0.83 AS is_verified,
  joined,
  CASE WHEN random() < 0.85 THEN joined + random() * (now() - joined) ELSE NULL END AS last_login_at
FROM (
  SELECT
    g AS id,
    (100000 + (g * 7919) % 900000) AS suffix,
    base.ccode,
    base.city || ', ' || base.ccode AS region,
    (base.ippre || '.' || floor(random()*254+1)::int || '.' || floor(random()*254+1)::int || '.' || floor(random()*254+1)::int)::inet AS signup_ip,
    base.fn, base.ln, base.domain, base.uname_base,
    '2020-06-01'::timestamptz + random() * (now() - '2020-06-01'::timestamptz) AS joined
  FROM generate_series(1, 50000) g
  CROSS JOIN country_slots cs
  CROSS JOIN uname_parts up
  CROSS JOIN LATERAL (
    SELECT cs.slots[1 + floor(random() * cardinality(cs.slots))::int] AS ccode
  ) pick
  JOIN countries co ON co.code = pick.ccode
  JOIN name_pools np ON np.locale_group = co.locale_group
  CROSS JOIN LATERAL (
    SELECT
      pick.ccode,
      np.first_names[1 + floor(random() * cardinality(np.first_names))::int] AS fn,
      np.last_names[1 + floor(random() * cardinality(np.last_names))::int] AS ln,
      np.domains[1 + floor(random() * cardinality(np.domains))::int] AS domain,
      np.ip_prefixes[1 + floor(random() * cardinality(np.ip_prefixes))::int] AS ippre,
      co.cities[1 + floor(random() * cardinality(co.cities))::int] AS city,
      up.adj[1 + floor(random() * cardinality(up.adj))::int]
        || up.noun[1 + floor(random() * cardinality(up.noun))::int] AS uname_base
  ) base
) t;

-- ============================================================================
-- Products (8,000 mods)
-- ============================================================================
\echo '=== Seeding 8,000 products ==='

CREATE TEMP TABLE cat_nouns (category text PRIMARY KEY, nouns text[] NOT NULL);
INSERT INTO cat_nouns VALUES
 ('Vehicles',        ARRAY['Handling Pack','Car Pack','Engine Sound Mod','Drift Kit','Motorcycle Pack','Vehicle Liveries','Speedometer Mod','Traffic Overhaul','Tuning Garage Pack','Emergency Vehicle Pack']),
 ('Weapons',         ARRAY['Weapon Pack','Gun Skin Bundle','Recoil Overhaul','Arsenal Expansion','Ballistics Mod','Melee Weapon Pack','Weapon Sound Pack','Attachments Pack','Sniper Kit','Crossbow Mod']),
 ('Maps',            ARRAY['Map Expansion','City Overhaul','Island Map','Terrain Remaster','Street Pack','Interior Mod','Region Expansion','Racing Circuit Pack','Wilderness Map','Underground Map']),
 ('Skins & Textures',ARRAY['Skin Pack','Outfit Bundle','Character Skins','Camo Pack','4K Texture Pack','Retexture Project','Uniform Pack','Cosmetic Bundle','Hair Pack','Tattoo Pack']),
 ('Graphics & ENB',  ARRAY['ENB Preset','Reshade Pack','Ray Tracing Mod','Lighting Overhaul','Weather Mod','Color Grading Pack','HDR Rebalance','Reflection Fix','Fog Overhaul','Cinematic Preset']),
 ('Gameplay',        ARRAY['AI Overhaul','Economy Mod','Difficulty Tweaks','Physics Mod','Realism Pack','Survival Overhaul','Progression Rebalance','Damage Overhaul','Stealth Rework','Companion AI Mod']),
 ('Scripts',         ARRAY['Script Hook','Mission Generator','Heist Scripts','Automation Suite','Event Spawner','Mod Menu Framework','Cheat Console','Save Editor','Replay Toolkit','Server Admin Tools']),
 ('Audio',           ARRAY['Sound Pack','Radio Expansion','Voice Pack','Ambience Overhaul','Music Replacer','SFX Remaster','Engine Audio Pack','Crowd Audio Mod','OST Integration','Dolby Mix']),
 ('UI & HUD',        ARRAY['HUD Overhaul','Menu Redesign','Minimap Mod','Crosshair Pack','Inventory UI Rework','Font Pack','Damage Indicator Mod','Radar Mod','Loadout Menu','Photo Mode UI']),
 ('Characters',      ARRAY['Character Models','Animation Pack','Face Textures','NPC Overhaul','Player Model Pack','Facial Rig Mod','Emotes Pack','Ped Variation Mod','Hair Physics Mod','Body Presets']);

CREATE TEMP TABLE game_slots AS
SELECT array_agg(id ORDER BY random()) AS slots
FROM (
  SELECT gm.id
  FROM games gm
  CROSS JOIN LATERAL generate_series(1, greatest(1, round(gm.mod_weight * 10)::int)) g
) s;

INSERT INTO products (id, game_id, category_id, creator_id, name, slug, description, price_usd,
                      version, file_size_mb, is_active, created_at)
SELECT
  g AS id,
  game_id,
  cat_id,
  1 + floor(random() * 600)::int,
  pname,
  lower(regexp_replace(pname, '[^a-zA-Z0-9]+', '-', 'g')) || '-' || g,
  'The ' || pname || ' is a community-favorite ' || lower(noun) || ' for ' || game_name || '. '
    || f1 || ' ' || f2,
  round((1.99 + power(random(), 1.7) * 38)::numeric, 2),
  (1 + floor(random() * 4))::int || '.' || floor(random() * 10)::int || '.' || floor(random() * 10)::int,
  round((5 + power(random(), 2.5) * 4000)::numeric, 1),
  random() < 0.97,
  '2020-01-01'::timestamptz + random() * (now() - '2020-01-01'::timestamptz)
FROM (
  SELECT
    gen.g,
    gen.game_id,
    gen.cat_id,
    gm.name AS game_name,
    n.noun,
    gm.short_name || ' ' || gen.adj || ' ' || n.noun
      || CASE WHEN random() < 0.35 THEN ' Vol. ' || (1 + floor(random() * 9))::int ELSE '' END AS pname,
    (ARRAY[
      'Easy installation with our auto-installer.',
      'Compatible with the latest game patch.',
      'Includes free lifetime updates.',
      'Optimized for minimal performance impact.',
      'Ships with detailed documentation.',
      'Works in single-player and supported online modes.',
      'Supports all official DLC content.',
      'Backed by a 30-day refund policy.'
    ])[1 + floor(random() * 8)::int] AS f1,
    (ARRAY[
      'Used by over 100k players worldwide.',
      'Featured on multiple modding showcases.',
      'Maintained actively by the author.',
      'Rated highly by the community.',
      'Regular balance patches included.',
      'Extensively tested on all platforms.'
    ])[1 + floor(random() * 6)::int] AS f2
  FROM (
    SELECT
      g,
      gs.slots[1 + floor(random() * cardinality(gs.slots))::int] AS game_id,
      1 + floor(random() * 10)::int AS cat_id,
      (ARRAY[
        'Realistic','Ultimate','Enhanced','HD','4K','Immersive','Tactical','Advanced','Custom','Retro',
        'Neon','Dark','Elite','Pro','Next-Gen','Vintage','Stealth','Dynamic','Precision','Chaos',
        'Apex','Prime','Titan','Quantum','Hyper'
      ])[1 + floor(random() * 25)::int] AS adj
    FROM generate_series(1, 8000) g
    CROSS JOIN game_slots gs
  ) gen
  JOIN games gm ON gm.id = gen.game_id
  JOIN categories c ON c.id = gen.cat_id
  JOIN cat_nouns cn ON cn.category = c.name
  CROSS JOIN LATERAL (
    SELECT cn.nouns[1 + floor(random() * cardinality(cn.nouns))::int] AS noun
  ) n
) t;

-- ============================================================================
-- Order generation skeleton: one row per ORDER (~150-160k)
-- ============================================================================
\echo '=== Building order skeleton (0-16 orders/user, skewed to 0) ==='

CREATE TEMP TABLE order_gen AS
WITH cum AS (
  SELECT ARRAY[0.306,0.478,0.593,0.679,0.746,0.798,0.841,0.875,0.904,0.928,0.947,0.962,0.974,0.983,0.991,0.996,1.0] AS w
),
per_user AS (
  SELECT u.id AS user_id, u.country_code, u.region, u.signup_ip, u.created_at AS user_created,
         co.currency, co.fx_rate, co.vat_rate, co.buyer_propensity,
         (SELECT count(*) FROM cum, unnest(cum.w) v
          WHERE (1 - power(1 - random(), co.buyer_propensity)) >= v) AS n_orders
  FROM users u
  JOIN countries co ON co.code = u.country_code
)
SELECT
  row_number() OVER () AS order_id,
  pu.user_id, pu.country_code, pu.currency, pu.fx_rate, pu.vat_rate, pu.region,
  o.created_at,
  CASE WHEN r_status < 0.88 THEN 'completed'
       WHEN r_status < 0.93 THEN 'refunded'
       WHEN r_status < 0.97 THEN 'failed'
       ELSE 'pending' END AS status,
  CASE WHEN r_pm < 0.55 THEN 'stripe'
       WHEN r_pm < 0.85 THEN 'paypal'
       ELSE 'crypto' END AS payment_method,
  CASE WHEN r_items < 0.50 THEN 1
       WHEN r_items < 0.75 THEN 2
       WHEN r_items < 0.88 THEN 3
       WHEN r_items < 0.96 THEN 4
       ELSE 5 END AS n_items,
  CASE WHEN random() < 0.70 THEN pu.signup_ip
       ELSE (floor(random()*200+27)::int || '.' || floor(random()*254+1)::int || '.' || floor(random()*254+1)::int || '.' || floor(random()*254+1)::int)::inet
  END AS ip_address
FROM per_user pu
CROSS JOIN LATERAL generate_series(1, pu.n_orders) k
CROSS JOIN LATERAL (
  SELECT random() AS r_status, random() AS r_pm, random() AS r_items,
         greatest(pu.user_created, timestamptz '2022-01-01') AS base
) r
CROSS JOIN LATERAL (
  SELECT
    CASE
      WHEN mode < 0.25 AND make_timestamptz(yr, 11, 24, 0, 0, 0) BETWEEN r.base AND now() - interval '34 days'
        THEN make_timestamptz(yr, 11, 24, 0, 0, 0) + random() * interval '34 days'
      WHEN mode < 0.35 AND make_timestamptz(yr, 6, 25, 0, 0, 0) BETWEEN r.base AND now() - interval '15 days'
        THEN make_timestamptz(yr, 6, 25, 0, 0, 0) + random() * interval '15 days'
      ELSE r.base + random() * (now() - r.base)
    END AS created_at
  FROM (SELECT random() AS mode, 2022 + floor(random() * 5)::int AS yr) m
) o;

-- ============================================================================
-- Orders + order items + totals
-- ============================================================================
\echo '=== Inserting orders ==='

INSERT INTO orders (id, user_id, status, payment_method, currency, subtotal, tax, total, ip_address, region, created_at)
SELECT order_id, user_id, status, payment_method, currency, 0, 0, 0, ip_address, region, created_at
FROM order_gen;

\echo '=== Inserting order items (1-5 per order) ==='

INSERT INTO order_items (id, order_id, product_id, unit_price, quantity, discount_pct, final_price)
SELECT
  row_number() OVER () AS id,
  og.order_id,
  p.id AS product_id,
  p.price_local AS unit_price,
  1 AS quantity,
  d.discount_pct,
  round(p.price_local * (1 - d.discount_pct / 100.0), 2) AS final_price
FROM order_gen og
CROSS JOIN LATERAL generate_series(1, og.n_items) k
CROSS JOIN LATERAL (
  SELECT id, round((price_usd * og.fx_rate)::numeric, 2) AS price_local
  FROM products
  WHERE id = 1 + floor(power(random(), 2.2) * 8000)::int
) p
CROSS JOIN LATERAL (
  SELECT CASE WHEN r < 0.70 THEN 0
              WHEN r < 0.85 THEN 10 + floor(random() * 16)::int
              WHEN r < 0.95 THEN 30 + floor(random() * 21)::int
              ELSE 60 + floor(random() * 21)::int END AS discount_pct
  FROM (SELECT random() AS r) x
) d;

\echo '=== Rolling up order totals ==='

UPDATE orders o
SET subtotal = s.sub,
    tax      = s.tx,
    total    = s.sub + s.tx
FROM (
  SELECT og.order_id,
         round(sum(oi.final_price), 2) AS sub,
         round(sum(oi.final_price) * og.vat_rate, 2) AS tx
  FROM order_items oi
  JOIN order_gen og ON og.order_id = oi.order_id
  GROUP BY og.order_id, og.vat_rate
) s
WHERE o.id = s.order_id;

-- ============================================================================
-- Payments (1 per order)
-- ============================================================================
\echo '=== Inserting payments ==='

INSERT INTO payments (id, order_id, provider, provider_ref, status, card_brand, card_last4, crypto_coin, amount, currency, created_at)
SELECT
  o.id,
  o.id,
  o.payment_method,
  CASE o.payment_method
    WHEN 'stripe' THEN 'ch_' || lower(substr(md5(o.id::text || ':stripe'), 1, 24))
    WHEN 'paypal' THEN 'PAYID-M' || upper(substr(md5(o.id::text || ':paypal'), 1, 12))
    ELSE '0x' || lower(substr(md5(o.id::text || ':c1') || md5(o.id::text || ':c2'), 1, 64))
  END,
  CASE o.status
    WHEN 'completed' THEN 'succeeded'
    WHEN 'refunded'  THEN 'refunded'
    WHEN 'failed'    THEN 'failed'
    ELSE 'processing'
  END,
  CASE WHEN o.payment_method = 'stripe'
       THEN (ARRAY['visa','visa','visa','mastercard','mastercard','amex'])[1+floor(random()*6)::int] END,
  CASE WHEN o.payment_method = 'stripe'
       THEN lpad(floor(random()*10000)::int::text, 4, '0') END,
  CASE WHEN o.payment_method = 'crypto'
       THEN (ARRAY['BTC','BTC','ETH','ETH','USDT','LTC'])[1+floor(random()*6)::int] END,
  o.total,
  o.currency,
  o.created_at + (random() * interval '3 minutes')
FROM orders o;

-- ============================================================================
-- Reviews (~110k, verified buyers only)
-- ============================================================================
\echo '=== Inserting reviews ==='

WITH bought AS (
  SELECT DISTINCT ON (o.user_id, oi.product_id)
         o.user_id, oi.product_id, o.id AS order_id, o.created_at AS bought_at
  FROM order_items oi
  JOIN orders o ON o.id = oi.order_id
  WHERE o.status IN ('completed','refunded') AND random() < 0.48
  ORDER BY o.user_id, oi.product_id, o.created_at
),
picked AS (
  SELECT * FROM bought ORDER BY random() LIMIT 110000
)
INSERT INTO reviews (id, user_id, product_id, order_id, rating, title, body, helpful_count, created_at)
SELECT
  row_number() OVER (),
  user_id,
  product_id,
  order_id,
  rating,
  title,
  s1 || ' ' || s2,
  floor(power(random(), 3) * 240)::int,
  least(bought_at + random() * interval '120 days' + interval '1 day', now())
FROM (
  SELECT b.*,
         CASE WHEN r < 0.45 THEN 5 WHEN r < 0.70 THEN 4 WHEN r < 0.85 THEN 3 WHEN r < 0.93 THEN 2 ELSE 1 END AS rating
  FROM picked b CROSS JOIN LATERAL (SELECT random() AS r) x
) rb
JOIN LATERAL (
  SELECT
    CASE WHEN rb.rating >= 4 THEN (ARRAY['Absolute must-have','Best mod I own','Worth every cent','Insane quality','Perfect','10/10 would buy again','Transformed my game','Exactly as described','Top tier mod','Instant favorite'])[1+floor(random()*10)::int]
         WHEN rb.rating = 3 THEN (ARRAY['Decent but flawed','Good value on sale','Solid, needs updates','Mixed feelings','Not bad','Okay for the price'])[1+floor(random()*6)::int]
         ELSE (ARRAY['Broke my game','Do not buy','Outdated and abandoned','Refund requested','Crashes constantly','Not what was advertised'])[1+floor(random()*6)::int]
    END AS title,
    CASE WHEN rb.rating >= 4 THEN (ARRAY['Installation took two minutes and the difference is night and day.','Works flawlessly with the latest patch.','The attention to detail is unreal.','Performance impact is basically zero.','Support from the author is fast and helpful.','This is how modding should be done.','Blends in perfectly with the base game.','Every update keeps getting better.'])[1+floor(random()*8)::int]
         WHEN rb.rating = 3 THEN (ARRAY['Good concept but a few rough edges.','Had some conflicts with other mods.','Works, but setup was confusing.','Fine on sale, would not pay full price.','Missing a few features I expected.','Decent but the last update broke things temporarily.'])[1+floor(random()*6)::int]
         ELSE (ARRAY['Crashed my game within minutes of installing.','Has not been updated in over a year.','Does not work with the current patch at all.','Save your money, free alternatives are better.','Caused constant stuttering even on high-end hardware.','Support never responded to my ticket.'])[1+floor(random()*6)::int]
    END AS s1,
    (ARRAY['Would still recommend to fans of the genre.','Grab it on a sale and you will not regret it.','Hope the author keeps updating it.','Paired it with a few other mods and it shines.','Customer support answered within a day.','Refund process was painless at least.','Will update my review if it gets fixed.','Wish I had found it sooner.','Runs great on my mid-range rig.','The community around this mod is fantastic.'])[1+floor(random()*10)::int] AS s2
) ph ON true;

-- ============================================================================
-- Wishlists (~180k unique pairs)
-- ============================================================================
\echo '=== Inserting wishlists ==='

INSERT INTO wishlists (id, user_id, product_id, added_at)
SELECT row_number() OVER (), user_id, product_id, added_at
FROM (
  SELECT DISTINCT ON (uid, pid)
         uid AS user_id, pid AS product_id,
         u.created_at + random() * (now() - u.created_at) AS added_at
  FROM (
    SELECT 1 + floor(random() * 50000)::int AS uid,
           1 + floor(power(random(), 1.8) * 8000)::int AS pid
    FROM generate_series(1, 260000)
  ) pairs
  JOIN users u ON u.id = pairs.uid
  ORDER BY uid, pid, random()
) w
LIMIT 180000;

-- ============================================================================
-- Download events (volume driver)
-- ============================================================================
\echo '=== Inserting download events (this takes a bit) ==='

CREATE TEMP TABLE user_agents AS SELECT a FROM (VALUES
 ('Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36'),
 ('Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:127.0) Gecko/20100101 Firefox/127.0'),
 ('Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15'),
 ('Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36'),
 ('Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36'),
 ('Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0'),
 ('Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36 Edg/126.0.0.0'),
 ('Mozilla/5.0 (Linux; Android 14; Pixel 8 Pro) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Mobile Safari/537.36'),
 ('Mozilla/5.0 (Linux; Android 14; SM-S928B) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Mobile Safari/537.36'),
 ('Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1'),
 ('Mozilla/5.0 (iPad; CPU OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1'),
 ('ModStore-Launcher/2.4.1 (Windows; x64)'),
 ('ModStore-Launcher/2.4.1 (macOS; arm64)'),
 ('ModStore-Launcher/2.3.8 (Linux; x64)')
) AS v(a);

INSERT INTO download_events (id, order_item_id, user_id, ip_address, region, user_agent, downloaded_at)
SELECT
  row_number() OVER (),
  oi.id,
  o.user_id,
  CASE WHEN random() < 0.75 THEN u.signup_ip
       ELSE (floor(random()*200+27)::int || '.' || floor(random()*254+1)::int || '.' || floor(random()*254+1)::int || '.' || floor(random()*254+1)::int)::inet
  END,
  o.region,
  ua.a,
  least(o.created_at + random() * interval '45 days', now())
FROM order_items oi
JOIN orders o ON o.id = oi.order_id
JOIN users u ON u.id = o.user_id
CROSS JOIN LATERAL generate_series(1, 1 + floor(random() * 3.2)::int) d
JOIN LATERAL (SELECT a FROM user_agents ORDER BY random() LIMIT 1) ua ON true
WHERE o.status = 'completed';

-- ============================================================================
-- Constraints & indexes (after bulk load)
-- ============================================================================
\echo '=== Adding constraints and indexes ==='

ALTER TABLE users ADD CONSTRAINT users_country_fk FOREIGN KEY (country_code) REFERENCES countries(code);
ALTER TABLE creators ADD CONSTRAINT creators_country_fk FOREIGN KEY (country_code) REFERENCES countries(code);
ALTER TABLE products ADD CONSTRAINT products_game_fk FOREIGN KEY (game_id) REFERENCES games(id);
ALTER TABLE products ADD CONSTRAINT products_category_fk FOREIGN KEY (category_id) REFERENCES categories(id);
ALTER TABLE products ADD CONSTRAINT products_creator_fk FOREIGN KEY (creator_id) REFERENCES creators(id);
ALTER TABLE orders ADD CONSTRAINT orders_user_fk FOREIGN KEY (user_id) REFERENCES users(id);
ALTER TABLE order_items ADD CONSTRAINT oi_order_fk FOREIGN KEY (order_id) REFERENCES orders(id);
ALTER TABLE order_items ADD CONSTRAINT oi_product_fk FOREIGN KEY (product_id) REFERENCES products(id);
ALTER TABLE payments ADD CONSTRAINT payments_order_fk FOREIGN KEY (order_id) REFERENCES orders(id);
ALTER TABLE reviews ADD CONSTRAINT reviews_user_fk FOREIGN KEY (user_id) REFERENCES users(id);
ALTER TABLE reviews ADD CONSTRAINT reviews_product_fk FOREIGN KEY (product_id) REFERENCES products(id);
ALTER TABLE reviews ADD CONSTRAINT reviews_order_fk FOREIGN KEY (order_id) REFERENCES orders(id);
ALTER TABLE wishlists ADD CONSTRAINT wl_user_fk FOREIGN KEY (user_id) REFERENCES users(id);
ALTER TABLE wishlists ADD CONSTRAINT wl_product_fk FOREIGN KEY (product_id) REFERENCES products(id);
ALTER TABLE download_events ADD CONSTRAINT de_item_fk FOREIGN KEY (order_item_id) REFERENCES order_items(id);
ALTER TABLE download_events ADD CONSTRAINT de_user_fk FOREIGN KEY (user_id) REFERENCES users(id);

ALTER TABLE users ADD CONSTRAINT users_username_key UNIQUE (username);
ALTER TABLE users ADD CONSTRAINT users_email_key UNIQUE (email);
ALTER TABLE reviews ADD CONSTRAINT reviews_user_product_key UNIQUE (user_id, product_id);
ALTER TABLE wishlists ADD CONSTRAINT wl_user_product_key UNIQUE (user_id, product_id);
ALTER TABLE payments ADD CONSTRAINT payments_order_key UNIQUE (order_id);

CREATE INDEX idx_users_country ON users(country_code);
CREATE INDEX idx_users_created ON users(created_at);
CREATE INDEX idx_products_game ON products(game_id);
CREATE INDEX idx_products_category ON products(category_id);
CREATE INDEX idx_products_price ON products(price_usd);
CREATE INDEX idx_orders_user ON orders(user_id);
CREATE INDEX idx_orders_created ON orders(created_at);
CREATE INDEX idx_orders_status ON orders(status);
CREATE INDEX idx_orders_pm ON orders(payment_method);
CREATE INDEX idx_oi_order ON order_items(order_id);
CREATE INDEX idx_oi_product ON order_items(product_id);
CREATE INDEX idx_reviews_product ON reviews(product_id);
CREATE INDEX idx_wishlists_user ON wishlists(user_id);
CREATE INDEX idx_de_item ON download_events(order_item_id);
CREATE INDEX idx_de_user ON download_events(user_id);
CREATE INDEX idx_de_downloaded ON download_events(downloaded_at);

ANALYZE;

-- ============================================================================
-- Verification
-- ============================================================================
\echo '=== Verification ==='

SELECT 'countries' AS table_name, count(*) FROM countries
UNION ALL SELECT 'games', count(*) FROM games
UNION ALL SELECT 'categories', count(*) FROM categories
UNION ALL SELECT 'creators', count(*) FROM creators
UNION ALL SELECT 'users', count(*) FROM users
UNION ALL SELECT 'products', count(*) FROM products
UNION ALL SELECT 'orders', count(*) FROM orders
UNION ALL SELECT 'order_items', count(*) FROM order_items
UNION ALL SELECT 'payments', count(*) FROM payments
UNION ALL SELECT 'reviews', count(*) FROM reviews
UNION ALL SELECT 'wishlists', count(*) FROM wishlists
UNION ALL SELECT 'download_events', count(*) FROM download_events
ORDER BY table_name;

SELECT pg_size_pretty(pg_database_size(current_database())) AS db_size;

SELECT count(*) AS order_date_violations
FROM orders o JOIN users u ON u.id = o.user_id
WHERE o.created_at < u.created_at OR o.created_at < '2022-01-01'::timestamptz;

SELECT min(created_at) AS first_order, max(created_at) AS last_order FROM orders;

SELECT payment_method, count(*), round(100.0 * count(*) / sum(count(*)) OVER (), 1) AS pct
FROM orders GROUP BY payment_method ORDER BY 2 DESC;
