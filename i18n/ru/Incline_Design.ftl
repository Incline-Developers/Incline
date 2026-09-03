# Incline — русский каталог сообщений.
#
# Может быть неполным: отсутствующие сообщения берутся из английского
# (`i18n/en/Incline.ftl`). Идентификаторы слева от `=` и имена
# аргументов ({ $... }) менять нельзя — переводится только текст справа.

## Общее

common-cancel = Отмена
common-clear = Очистить
common-close = Закрыть
common-color = Цвет
common-fill = Заливка

## Настройки — Интерфейс

settings-language = Язык
settings-language-system = Системный
settings-language-restart-hint = Смена языка вступит в силу при следующем запуске Incline.

## Меню — Файл

menu-file = Файл
menu-file-save-project = Сохранить проект
menu-file-save-project-as = Сохранить проект как...
menu-file-new-project = Новый проект...
menu-file-open-project = Открыть проект...
menu-file-open-recent = Последние проекты
menu-file-show-in-explorer = Показать в Проводнике
menu-file-show-in-folder = Открыть папку с файлом
menu-file-import = Импорт...
menu-file-export = Экспорт...
menu-file-export-viewport-image = Экспорт изображения области просмотра...
menu-file-export-engineering-drawing = Экспорт чертежа...
menu-file-about = О программе { $app }...
menu-file-exit = Выйти из приложения

## Меню — Вид

menu-view = Вид


## Workspaces

ws-production = Производство
ws-drill-and-blast = БВР
ws-geology = Геология


## Menubars

ws-menubar-design = Проектирование
ws-menubar-triangulation = Триангуляция
ws-menubar-raster = Растер
ws-menubar-point-cloud = Облака точек
ws-menubar-block-model = Блочная модель
ws-menubar-drillholes = Скважины
ws-menubar-active-layer = Слой:

## Диалоги переименования и удаления

dialog-rename-title = Переименовать: { $kind }
dialog-rename-field = Новое имя
dialog-rename-field-hint = Обязательно
dialog-rename-submit = Переименовать
dialog-delete-title = Удалить: { $kind }
dialog-delete-confirm =
    Удалить «{ $name }» из проекта?
    Это действие нельзя отменить.

## Диалог «Создать триангуляцию»

tri-create-title = Создать триангуляцию
tri-create-help = Щёлкайте по объектам в области просмотра, чтобы выбрать или снять выбор. Для рамочного выбора протяните курсор.
tri-create-type-label = Тип триангуляции
tri-create-type-help =
    «Открытая поверхность» создаёт полотно рельефного типа. «Тело» создаёт
    полностью замкнутую сетку и требует входных данных, образующих
    герметичную границу.
tri-create-output-name = Имя результата
tri-create-output-name-help = Имя, присваиваемое созданной триангуляции.
tri-create-output-name-hint = имя триангуляции
tri-create-run = Триангулировать

tri-selection-none = Объекты ещё не выбраны.
tri-selection-selected = Выбрано: { $summary }

tri-type-open-surface = Открытая поверхность
tri-type-solid-closed = Тело — полностью замкнутое

tri-count-polylines =
    { $count ->
        [one] { $count } полилиния
        [few] { $count } полилинии
       *[other] { $count } полилиний
    }
tri-count-strings =
    { $count ->
        [one] { $count } линия
        [few] { $count } линии
       *[other] { $count } линий
    }
tri-count-points =
    { $count ->
        [one] { $count } точка
        [few] { $count } точки
       *[other] { $count } точек
    }
tri-count-texts =
    { $count ->
        [one] { $count } текстовый объект
        [few] { $count } текстовых объекта
       *[other] { $count } текстовых объектов
    }
tri-count-objects =
    { $count ->
        [one] { $count } объект
        [few] { $count } объекта
       *[other] { $count } объектов
    }


about-read-full-licence = Подробности о лицензии ↗
about-source-code = Исходный код
about-website = Сайт